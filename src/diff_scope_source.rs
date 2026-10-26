//! Original diff scope analysis uses a retained source snapshot, never the
//! interleaved display rows or a freshly rebuilt Rope on the drawing thread.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Weak, mpsc},
};

use crate::{
    animation_structural_scope::{StructuralScopeIndex, StructuralScopeRequest},
    app::App,
    background::CancellationToken,
    editor::Editor,
};

#[derive(Clone)]
struct SourceKey {
    base: Weak<str>,
    path: Option<PathBuf>,
    language: Option<String>,
    rules: Arc<crate::scope_bubble_config::Rules>,
}

impl SourceKey {
    fn new(base: &Arc<str>, path: Option<&Path>, language: Option<&str>) -> Self {
        Self {
            base: Arc::downgrade(base),
            path: path.map(Path::to_path_buf),
            language: language.map(str::to_owned),
            rules: crate::scope_bubble_config::Rules::empty(),
        }
    }

    fn matches(&self, base: &Arc<str>, path: Option<&Path>, language: Option<&str>) -> bool {
        std::ptr::eq(self.base.as_ptr(), Arc::as_ptr(base))
            && self.path.as_deref() == path
            && self.language.as_deref() == language
    }
}

struct BuiltSource {
    rope: ropey::Rope,
    index: Option<Arc<StructuralScopeIndex>>,
    navigation_id: u64,
    version: u64,
    wrap_revision: u64,
}

struct CachedSource {
    key: SourceKey,
    editor: Editor,
    index: Option<Arc<StructuralScopeIndex>>,
}

struct PendingSource {
    key: SourceKey,
    cancel: CancellationToken,
    receiver: mpsc::Receiver<Option<BuiltSource>>,
}

impl Drop for PendingSource {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Default)]
pub(crate) struct DiffScopeSource {
    pub(crate) target_paint: crate::ui_editor_animations::DiffScopePaintState,
    pub(crate) original_paint: crate::ui_editor_animations::DiffScopePaintState,
    original_animation: Option<(SourceKey, u64)>,
    cached: Option<CachedSource>,
    pending: Option<PendingSource>,
}

impl DiffScopeSource {
    /// Cursor effects need a revision identity, not its complete parsed source.
    /// Keep it independent of structural worker completion and effect toggles.
    pub(crate) fn original_animation_id(
        &mut self,
        base: &Arc<str>,
        syntax_path: Option<&Path>,
        language: Option<&str>,
    ) -> u64 {
        if let Some((key, id)) = &self.original_animation
            && key.matches(base, syntax_path, language)
        {
            return *id;
        }
        let id = Editor::allocate_navigation_id();
        self.original_animation = Some((SourceKey::new(base, syntax_path, language), id));
        id
    }

    fn invalidate_animation_source(&mut self, source: Option<SourceRef<'_>>) -> Option<u64> {
        if self
            .original_animation
            .as_ref()
            .is_some_and(|(key, _)| source.is_none_or(|source| !key.matches(source.base, source.path, source.language)))
        {
            return self.original_animation.take().map(|(_, id)| id);
        }
        None
    }

    pub(crate) fn forget_original_view(&self, animations: &mut crate::animations::AnimationState) {
        if let Some((_, id)) = &self.original_animation {
            animations.forget_view(*id);
        }
        if let Some(cached) = &self.cached {
            animations.forget_view(cached.editor.navigation_id);
        }
    }

    pub(crate) fn editor_and_index(
        &self,
        base: &Arc<str>,
        syntax_path: Option<&Path>,
        language: Option<&str>,
    ) -> Option<(&Editor, Option<&StructuralScopeIndex>)> {
        let cached = self.cached.as_ref()?;
        cached
            .key
            .matches(base, syntax_path, language)
            .then_some((&cached.editor, cached.index.as_deref()))
    }

    pub(crate) fn editor_and_index_for_rules(
        &self, base: &Arc<str>, path: Option<&Path>, language: Option<&str>,
        rules: &Arc<crate::scope_bubble_config::Rules>,
    ) -> Option<(&Editor, Option<&StructuralScopeIndex>)> {
        let (editor, index) = self.editor_and_index(base, path, language)?;
        Arc::ptr_eq(&editor.scope_bubble_rules, rules).then_some((editor, index))
    }

    #[cfg(test)]
    pub(crate) fn editor_and_index_mut(
        &mut self,
        base: &Arc<str>,
        syntax_path: Option<&Path>,
        language: Option<&str>,
    ) -> Option<(&mut Editor, Option<&StructuralScopeIndex>)> {
        let cached = self.cached.as_mut()?;
        cached
            .key
            .matches(base, syntax_path, language)
            .then_some((&mut cached.editor, cached.index.as_deref()))
    }

    /// Cursor changes reuse the same source/editor identity and lexical index.
    pub(crate) fn set_cursor(&mut self, position: (usize, usize)) {
        if let Some(cached) = self.cached.as_mut() {
            let editor = &mut cached.editor;
            editor.cursor_line = position.0.min(editor.line_count().saturating_sub(1));
            editor.cursor_col = position.1.min(editor.line_len(editor.cursor_line));
            editor.selection_anchor = None;
            editor.desired_col = None;
        }
    }

    fn invalidate(&mut self, source: Option<SourceRef<'_>>) {
        if self
            .cached
            .as_ref()
            .is_some_and(|cached| source.is_none_or(|source| !source.matches(&cached.key)))
        {
            self.cached = None;
            self.original_paint = Default::default();
        }
        if source.is_none() {
            self.original_paint = Default::default();
            self.target_paint = Default::default();
        }
        if let Some(pending) = self.pending.as_ref()
            && source.is_none_or(|source| !source.matches(&pending.key))
        {
            // Keep the slot until the worker acknowledges cancellation. Rapid
            // source changes cannot accumulate queued obsolete analyses.
            pending.cancel.cancel();
        }
    }

    fn poll(&mut self, source: Option<SourceRef<'_>>) -> bool {
        self.invalidate(source);
        let Some(pending) = self.pending.as_ref() else {
            return false;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let pending = self.pending.take().unwrap();
        if pending.cancel.is_cancelled()
            || source.is_none_or(|source| !source.matches(&pending.key))
        {
            // Rearm refresh after the old worker releases its slot, even if
            // no more input arrives for the newly focused source.
            return source.is_some();
        }
        let Some(built) = result else {
            return false;
        };
        let mut editor = Editor::new();
        editor.rope = built.rope;
        editor.navigation_id = built.navigation_id;
        editor.content_version = built.version;
        editor.wrap_revision = built.wrap_revision;
        editor.file_path = pending.key.path.clone();
        editor.language_override = pending.key.language.clone();
        editor.scope_bubble_rules = pending.key.rules.clone();
        editor.read_only = true;
        editor.file_size = editor.rope.len_bytes() as u64;
        debug_assert!(
            built
                .index
                .as_ref()
                .is_none_or(|index| index.matches(&editor))
        );
        self.cached = Some(CachedSource {
            key: pending.key.clone(),
            editor,
            index: built.index,
        });
        true
    }

    fn ready_or_pending(&self, source: SourceRef<'_>) -> bool {
        self.pending.is_some()
            || self
                .cached
                .as_ref()
                .is_some_and(|cached| source.matches(&cached.key))
    }
}

#[derive(Clone, Copy)]
struct SourceRef<'a> {
    base: &'a Arc<str>,
    path: Option<&'a Path>,
    language: Option<&'a str>,
    rules: &'a Arc<crate::scope_bubble_config::Rules>,
}

impl SourceRef<'_> {
    fn key(self) -> SourceKey {
        let mut key = SourceKey::new(self.base, self.path, self.language);
        key.rules = self.rules.clone();
        key
    }

    fn matches(self, key: &SourceKey) -> bool {
        key.matches(self.base, self.path, self.language) && Arc::ptr_eq(&key.rules, self.rules)
    }
}

fn animation_source(app: &App) -> Option<SourceRef<'_>> {
    if !app.should_render_diff_for_active_editor() {
        return None;
    }
    let editor = app.editor().filter(|editor| !editor.bg_loading)?;
    Some(SourceRef {
        base: app.compare_base_content.as_ref()?,
        path: editor.syntax_file_path(),
        language: editor.syntax_language_token(),
        rules: &editor.scope_bubble_rules,
    })
}

fn source(app: &App) -> Option<SourceRef<'_>> {
    app.editor().is_some_and(|editor| app.animation_settings.scope_enabled_for(editor))
        .then(|| animation_source(app))
        .flatten()
}

/// Active motion follows the caret's revision. The tab underline retains the
/// actual editor identity independently in the chrome renderer.
pub(crate) fn animation_view_id(app: &mut App) -> Option<u64> {
    let target_id = app.editor().map(|editor| editor.navigation_id);
    if !app.animation_settings.editor_effects_enabled()
        || app.rendering_inactive_editor
        || !app
            .diff_scope_cursor()
            .is_some_and(|(original, _)| original)
    {
        return target_id;
    }
    let mut state = std::mem::take(&mut app.diff_scope_source);
    let current = animation_source(app);
    let retired = state.invalidate_animation_source(current);
    let id = current
        .map(|source| state.original_animation_id(source.base, source.path, source.language));
    if let Some(retired) = retired {
        app.animations.forget_view(retired);
    }
    app.diff_scope_source = state;
    id.or(target_id)
}

fn build_source_with_rules(
    base: &str,
    path: Option<PathBuf>,
    language: Option<String>,
    rules: Arc<crate::scope_bubble_config::Rules>,
    cancelled: impl Fn() -> bool,
) -> Option<BuiltSource> {
    if cancelled() {
        return None;
    }
    let mut editor = Editor::new();
    editor.rope = ropey::Rope::from_str(base);
    editor.file_path = path;
    editor.language_override = language;
    editor.scope_bubble_rules = rules;
    editor.read_only = true;
    if cancelled() {
        return None;
    }
    let index = match StructuralScopeRequest::from_editor(&editor) {
        Some(request) => Some(Arc::new(request.build(None, &cancelled)?)),
        None => None,
    };
    if cancelled() {
        return None;
    }
    Some(BuiltSource {
        rope: editor.rope,
        index,
        navigation_id: editor.navigation_id,
        version: editor.content_version,
        wrap_revision: editor.wrap_revision,
    })
}

#[cfg(test)]
fn build_source(base: &str, path: Option<PathBuf>, language: Option<String>, cancelled: impl Fn() -> bool) -> Option<BuiltSource> {
    build_source_with_rules(base, path, language, crate::scope_bubble_config::Rules::empty(), cancelled)
}

/// Poll every event-loop wake, since this private channel has no BackgroundResult.
pub(crate) fn poll(app: &mut App) -> bool {
    let mut state = std::mem::take(&mut app.diff_scope_source);
    if let Some(retired) = state.invalidate_animation_source(animation_source(app)) {
        app.animations.forget_view(retired);
    }
    let previous = state
        .cached
        .as_ref()
        .map(|cached| cached.editor.navigation_id);
    let changed = state.poll(source(app));
    if let Some(previous) = previous
        && state
            .cached
            .as_ref()
            .map(|cached| cached.editor.navigation_id)
            != Some(previous)
    {
        app.animations.forget_view(previous);
    }
    app.diff_scope_source = state;
    changed
}

/// Schedule at most one original-source analysis, only after original/deleted
/// focus is requested. A completed cache stays warm when focus returns to target.
pub(crate) fn refresh(app: &mut App) {
    let mut state = std::mem::take(&mut app.diff_scope_source);
    if let Some(retired) = state.invalidate_animation_source(animation_source(app)) {
        app.animations.forget_view(retired);
    }
    let previous = state
        .cached
        .as_ref()
        .map(|cached| cached.editor.navigation_id);
    let current = source(app);
    state.invalidate(current);
    if let Some(current) = current
        && app
            .diff_scope_cursor()
            .is_some_and(|(original, _)| original)
        && !state.ready_or_pending(current)
    {
        let key = current.key();
        let base = Arc::clone(current.base);
        let path = key.path.clone();
        let language = key.language.clone();
        let rules = key.rules.clone();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        let wake = app.background.wake_handle();
        let queued = app.background.pool_sender().send(Box::new(move || {
            let built = build_source_with_rules(&base, path, language, rules, || worker_cancel.is_cancelled());
            let _ = sender.send(built);
            wake.notify();
        }));
        if queued.is_ok() {
            state.pending = Some(PendingSource {
                key,
                cancel,
                receiver,
            });
        }
    }
    if let Some(previous) = previous
        && state
            .cached
            .as_ref()
            .map(|cached| cached.editor.navigation_id)
            != Some(previous)
    {
        app.animations.forget_view(previous);
    }
    app.diff_scope_source = state;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation_config::AnimationEffect;

    const BASE: &str = "fn original() {\n    if ready {\n        work();\n    }\n}\n";

    fn test_rules() -> &'static Arc<crate::scope_bubble_config::Rules> {
        static RULES: std::sync::OnceLock<Arc<crate::scope_bubble_config::Rules>> = std::sync::OnceLock::new();
        RULES.get_or_init(crate::scope_bubble_config::Rules::empty)
    }

    #[test]
    fn custom_scope_rules_apply_to_original_diff_and_retire_stale_workers() {
        use crate::scope_bubble_config::{Config, Delimiter, Rules, Syntax};
        let base: Arc<str> = "{\n call(\n value\n );\n}\n".into();
        let make_rules = |pairs| Rules::compile(&[("Custom".into(), Config {
            syntax: Syntax::Cpp, delimiters: Some(pairs), language_blocks: false,
        })].into());
        let braces = make_rules(vec![Delimiter::Braces]);
        let all = make_rules(Delimiter::ALL.to_vec());
        let original = SourceRef { base: &base, path: Some(Path::new("no-extension")), language: Some("Custom"), rules: &braces };
        let changed = SourceRef { rules: &all, ..original };
        let mut state = DiffScopeSource::default();
        let id = state.original_animation_id(&base, original.path, original.language);
        let built = |rules: Arc<Rules>| build_source_with_rules(&base, original.path.map(Path::to_path_buf), original.language.map(str::to_owned), rules, || false);
        let (sender, _) = pending(&mut state, original);
        sender.send(built(braces.clone())).unwrap();
        assert!(state.poll(Some(original)));
        state.set_cursor((2, 2));
        let (editor, index) = state.editor_and_index_for_rules(&base, original.path, original.language, &braces).unwrap();
        assert_eq!(index.unwrap().enclosing_scope(editor), Some(((0, 0), (4, 0))));
        assert!(state.editor_and_index_for_rules(&base, original.path, original.language, &all).is_none());
        assert!(state.invalidate_animation_source(Some(changed)).is_none());
        assert_eq!(state.original_animation_id(&base, original.path, original.language), id);
        let (sender, cancel) = pending(&mut state, original);
        state.invalidate(Some(changed));
        assert!(state.cached.is_none() && cancel.is_cancelled());
        sender.send(built(braces.clone())).unwrap();
        assert!(state.poll(Some(changed)));
        assert!(state.cached.is_none());
        let (sender, _) = pending(&mut state, changed);
        sender.send(built(all.clone())).unwrap();
        assert!(state.poll(Some(changed)));
        state.set_cursor((2, 2));
        let (editor, index) = state.editor_and_index_for_rules(&base, original.path, original.language, &all).unwrap();
        assert_eq!(index.unwrap().enclosing_scope(editor), Some(((1, 5), (3, 1))));
    }
    fn original<'a>(base: &'a Arc<str>) -> SourceRef<'a> {
        SourceRef {
            rules: test_rules(),
            base,
            path: Some(Path::new("example.rs")),
            language: Some("Rust"),
        }
    }

    fn pending(
        state: &mut DiffScopeSource,
        source: SourceRef<'_>,
    ) -> (mpsc::Sender<Option<BuiltSource>>, CancellationToken) {
        let (sender, receiver) = mpsc::channel();
        let cancel = CancellationToken::new();
        state.pending = Some(PendingSource {
            key: source.key(),
            cancel: cancel.clone(),
            receiver,
        });
        (sender, cancel)
    }

    fn complete(state: &mut DiffScopeSource, source: SourceRef<'_>) {
        let (sender, _) = pending(state, source);
        let built = build_source(
            source.base,
            source.path.map(Path::to_path_buf),
            source.language.map(str::to_owned),
            || false,
        )
        .unwrap();
        sender.send(Some(built)).ok().unwrap();
        assert!(state.poll(Some(source)));
    }

    #[test]
    fn scope_language_disable_retires_original_analysis_and_rejects_late_results() {
        let base: Arc<str> = Arc::from(BASE);
        let source = original(&base);
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(BASE);
        editor.file_path = source.path.map(Path::to_path_buf);
        editor.language_override = source.language.map(str::to_owned);
        app.tabs = vec![crate::app::Tab::new_file(editor)];
        app.active_tab = 0;
        app.diff_mode = crate::app::DiffMode::SideBySide;
        app.compare_base_content = Some(Arc::clone(&base));
        app.animation_settings.set(AnimationEffect::ScopeConnector, true);
        complete(&mut app.diff_scope_source, source);
        let (sender, cancel) = pending(&mut app.diff_scope_source, source);
        app.animation_settings.scope_disabled_languages = vec!["rust".into()];
        refresh(&mut app);
        assert!(cancel.is_cancelled());
        assert!(app.diff_scope_source.cached.is_none());
        sender.send(build_source(BASE, source.path.map(Path::to_path_buf),
            source.language.map(str::to_owned), || false)).ok();
        poll(&mut app);
        assert!(app.diff_scope_source.cached.is_none());
        assert!(app.diff_scope_source.pending.is_none());
    }

    #[test]
    fn original_motion_survives_scope_worker_replacement_but_retires_with_its_revision() {
        use crate::{
            animations::{Anchor, Snapshot},
            app::{DiffMode, Tab},
        };
        use ratatui::layout::Rect;
        use std::time::Instant;

        let base: Arc<str> = Arc::from(BASE);
        let source = original(&base);
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(BASE))];
        app.active_tab = 0;
        app.diff_mode = DiffMode::SideBySide;
        app.diff_target_path = None;
        app.compare_base_content = Some(Arc::clone(&base));
        let editor = app.editor_mut().unwrap();
        editor.file_path = source.path.map(Path::to_path_buf);
        editor.language_override = source.language.map(str::to_owned);
        app.animation_settings
            .set(AnimationEffect::CursorSmear, true);
        app.animation_settings.duration_ms = 1500;
        let id = app
            .diff_scope_source
            .original_animation_id(&base, source.path, source.language);
        let now = Instant::now();
        let area = Rect::new(0, 0, 40, 10);
        let anchors = [
            Anchor {
                line: 2,
                col: 8,
                x: 8,
                y: 2,
                width: 1,
                ch: 'w',
            },
            Anchor {
                line: 2,
                col: 14,
                x: 14,
                y: 2,
                width: 1,
                ch: ';',
            },
        ];
        for cursor in [(2, 8), (2, 14)] {
            app.animations.observe(
                Snapshot::read_only_motion(id, cursor, (0, 0, 0), area),
                &anchors,
                &app.animation_settings,
                now,
            );
        }
        assert!(
            app.animations
                .cursor_smear_active(id, &app.animation_settings, now)
        );

        for _ in 0..2 {
            app.animation_settings
                .set(AnimationEffect::ScopeConnector, true);
            let (sender, _) = pending(&mut app.diff_scope_source, source);
            sender
                .send(build_source(
                    BASE,
                    source.path.map(Path::to_path_buf),
                    source.language.map(str::to_owned),
                    || false,
                ))
                .ok()
                .unwrap();
            assert!(poll(&mut app));
            let (editor, index) = app
                .diff_scope_source
                .editor_and_index(&base, source.path, source.language)
                .unwrap();
            assert!(index.unwrap().matches(editor));
            assert_ne!(editor.navigation_id, id);
            assert_eq!(
                app.diff_scope_source
                    .original_animation_id(&base, source.path, source.language),
                id
            );
            assert!(
                app.animations
                    .cursor_smear_active(id, &app.animation_settings, now)
            );

            app.animation_settings
                .set(AnimationEffect::ScopeConnector, false);
            refresh(&mut app);
            assert_eq!(
                app.diff_scope_source
                    .original_animation_id(&base, source.path, source.language),
                id
            );
            assert!(
                app.animations
                    .cursor_smear_active(id, &app.animation_settings, now)
            );
        }

        let replacement: Arc<str> = Arc::from("fn replacement() {}\n");
        app.compare_base_content = Some(Arc::clone(&replacement));
        refresh(&mut app);
        assert!(
            !app.animations
                .cursor_smear_active(id, &app.animation_settings, now)
        );
        let replacement_id =
            app.diff_scope_source
                .original_animation_id(&replacement, source.path, source.language);
        assert_ne!(replacement_id, id);
        assert!(
            !app.animations
                .cursor_smear_active(replacement_id, &app.animation_settings, now)
        );
    }

    #[test]
    fn original_source_keeps_exact_worker_identity_and_warm_cursor_queries() {
        let base: Arc<str> = Arc::from(BASE);
        let source = original(&base);
        let target = Editor::new();
        let mut state = DiffScopeSource::default();
        complete(&mut state, source);
        let (editor, index) = state
            .editor_and_index(&base, source.path, source.language)
            .unwrap();
        let index = index.unwrap();
        let navigation_id = editor.navigation_id;
        let index_pointer = index as *const StructuralScopeIndex;
        assert_ne!(navigation_id, target.navigation_id);
        assert!(editor.read_only);
        assert!(index.matches(editor));
        assert_eq!(editor.rope.to_string(), BASE);
        assert_eq!(Arc::strong_count(&base), 1);

        for cursor in [(2, 10), (3, 4), (2, 0)] {
            state.set_cursor(cursor);
            let (editor, index) = state
                .editor_and_index(&base, source.path, source.language)
                .unwrap();
            let index = index.unwrap();
            assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
            assert_eq!(editor.navigation_id, navigation_id);
            assert_eq!(index as *const StructuralScopeIndex, index_pointer);
            assert!(index.matches(editor));
            assert_eq!(index.enclosing_scope(editor), Some(((1, 13), (3, 4))));
        }
    }

    #[test]
    fn original_source_requires_exact_arc_and_syntax_metadata() {
        let base: Arc<str> = Arc::from(BASE);
        let replacement: Arc<str> = Arc::from(BASE);
        let mut state = DiffScopeSource::default();
        complete(&mut state, original(&base));
        assert!(
            state
                .editor_and_index(&replacement, Some(Path::new("example.rs")), Some("Rust"))
                .is_none()
        );
        assert!(
            state
                .editor_and_index(&base, Some(Path::new("example.js")), Some("Rust"))
                .is_none()
        );
        assert!(
            state
                .editor_and_index(&base, Some(Path::new("example.rs")), Some("JavaScript"))
                .is_none()
        );
        state.invalidate(Some(original(&replacement)));
        assert!(state.cached.is_none());
        assert!(!state.ready_or_pending(original(&replacement)));
    }

    #[test]
    fn superseded_source_waits_for_ack_then_rearms_without_publishing_stale_result() {
        let base: Arc<str> = Arc::from(BASE);
        let replacement: Arc<str> = Arc::from("fn replacement() {}\n");
        let mut state = DiffScopeSource::default();
        let (sender, cancel) = pending(&mut state, original(&base));
        state.invalidate(Some(original(&replacement)));
        assert!(cancel.is_cancelled());
        assert!(state.ready_or_pending(original(&replacement)));
        assert!(!state.poll(Some(original(&replacement))));
        sender
            .send(build_source(
                BASE,
                Some(PathBuf::from("example.rs")),
                Some("Rust".into()),
                || false,
            ))
            .ok()
            .unwrap();
        assert!(state.poll(Some(original(&replacement))));
        assert!(state.cached.is_none());
        assert!(!state.ready_or_pending(original(&replacement)));
        complete(&mut state, original(&replacement));
        assert!(
            state
                .editor_and_index(&replacement, Some(Path::new("example.rs")), Some("Rust"))
                .is_some()
        );
    }

    #[test]
    fn cancelled_or_dropped_source_cannot_install() {
        let base: Arc<str> = Arc::from(BASE);
        let mut state = DiffScopeSource::default();
        let (sender, cancel) = pending(&mut state, original(&base));
        cancel.cancel();
        sender
            .send(build_source(
                BASE,
                Some(PathBuf::from("example.rs")),
                Some("Rust".into()),
                || false,
            ))
            .ok()
            .unwrap();
        assert!(state.poll(Some(original(&base))));
        assert!(state.cached.is_none());

        let (sender, _) = pending(&mut state, original(&base));
        sender.send(None).ok().unwrap();
        assert!(!state.poll(Some(original(&base))));
        assert!(!state.ready_or_pending(original(&base)));

        let (_, cancel) = pending(&mut state, original(&base));
        drop(state);
        assert!(cancel.is_cancelled());
        assert!(build_source(BASE, None, Some("Rust".into()), || true).is_none());
    }

    #[test]
    fn keyword_only_original_keeps_warm_editor_without_curly_index() {
        let base: Arc<str> = Arc::from("if ready\n  work\nend\n");
        let source = SourceRef {
            rules: test_rules(),
            base: &base,
            path: Some(Path::new("example.rb")),
            language: Some("Ruby"),
        };
        let mut state = DiffScopeSource::default();
        complete(&mut state, source);
        assert!(state.ready_or_pending(source));
        state.set_cursor((1, 2));
        let (editor, index) = state
            .editor_and_index(&base, source.path, source.language)
            .unwrap();
        assert!(index.is_none());
        assert_eq!(
            crate::animation_scope::keyword_scope(editor),
            Some(((0, 0), (2, 0)))
        );
        let identity = editor.navigation_id;
        state.set_cursor((1, 4));
        let (editor, index) = state
            .editor_and_index_mut(&base, source.path, source.language)
            .unwrap();
        assert_eq!(editor.navigation_id, identity);
        assert!(index.is_none());
        state.invalidate(None);
        assert!(state.cached.is_none());
        assert!(!state.ready_or_pending(source));
    }
}
