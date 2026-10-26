//! Git status is ordered by group, then path. Resolve visible geometry from
//! group boundaries; filtering, scrolling and collapsed groups never scan files.
use crate::app::GitFileChange;
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Merge,
    Staged,
    Unstaged,
    Untracked,
}

impl Group {
    pub const ALL: [Self; 4] = [Self::Merge, Self::Staged, Self::Unstaged, Self::Untracked];

    pub fn of(file: &GitFileChange) -> Self {
        if file.status == '!' {
            Self::Merge
        } else if file.staged {
            Self::Staged
        } else if file.status == '?' {
            Self::Untracked
        } else {
            Self::Unstaged
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "Merge Changes",
            Self::Staged => "Staged Changes",
            Self::Unstaged => "Unstaged Changes",
            Self::Untracked => "Untracked Files",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Visibility {
    pub untracked: bool,
}

impl Default for Visibility {
    fn default() -> Self {
        Self { untracked: true }
    }
}

impl Visibility {
    pub fn shows(self, group: Group) -> bool {
        match group {
            Group::Merge | Group::Staged | Group::Unstaged => true,
            Group::Untracked => self.untracked,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Section(Group),
    File(usize),
}

struct Section {
    group: Group,
    files: Range<usize>,
    shown: bool,
    collapsed: bool,
}

impl Section {
    fn header(&self) -> bool {
        self.shown && (!self.files.is_empty() || self.group == Group::Unstaged)
    }

    fn rows(&self) -> usize {
        usize::from(self.header())
            + if self.shown && !self.collapsed {
                self.files.len()
            } else {
                0
            }
    }
}

pub(crate) struct Layout {
    sections: [Section; 4],
}

impl Layout {
    pub fn new(files: &[GitFileChange], visibility: Visibility, collapsed: [bool; 4]) -> Self {
        let merge = files.partition_point(|file| file.status == '!');
        let staged = merge + files[merge..].partition_point(|file| file.staged);
        let tracked = staged + files[staged..].partition_point(|file| file.status != '?');
        let boundaries = [0, merge, staged, tracked, files.len()];
        Self {
            sections: std::array::from_fn(|index| Section {
                group: Group::ALL[index],
                files: boundaries[index]..boundaries[index + 1],
                shown: visibility.shows(Group::ALL[index]),
                collapsed: collapsed[index],
            }),
        }
    }

    pub fn count(&self, group: Group) -> usize {
        self.sections[group as usize].files.len()
    }

    pub fn file_count(&self) -> usize {
        self.sections
            .iter()
            .filter(|section| section.shown)
            .map(|section| section.files.len())
            .sum()
    }

    pub fn row_count(&self) -> usize {
        self.sections.iter().map(Section::rows).sum()
    }

    pub fn max_scroll(&self, visible: usize) -> usize {
        if visible == 0 {
            0
        } else {
            self.row_count().saturating_sub(visible)
        }
    }

    /// Offsets index rendered rows, so hidden/collapsed files consume no space.
    /// Jump directly into each range instead of skipping through offscreen files.
    pub fn rows(&self, mut offset: usize) -> impl Iterator<Item = Row> + '_ {
        self.sections.iter().flat_map(move |section| {
            let skip = offset.min(section.rows());
            offset = offset.saturating_sub(section.rows());
            let header = section.header();
            let start = if section.shown && !section.collapsed {
                section.files.start + skip.saturating_sub(usize::from(header))
            } else {
                section.files.end
            };
            (header && skip == 0)
                .then_some(Row::Section(section.group))
                .into_iter()
                .chain((start..section.files.end).map(Row::File))
        })
    }

    /// Keyboard navigation counts visible groups, including collapsed ones so
    /// navigating into a group can expand it, but never enters a hidden group.
    pub fn file_at(&self, mut ordinal: usize) -> Option<usize> {
        for section in &self.sections {
            if !section.shown {
                continue;
            }
            if ordinal < section.files.len() {
                return Some(section.files.start + ordinal);
            }
            ordinal = ordinal.saturating_sub(section.files.len());
        }
        None
    }

    pub fn file_ordinal(&self, index: usize) -> Option<usize> {
        let mut offset = 0;
        for section in &self.sections {
            if !section.shown {
                continue;
            }
            if section.files.contains(&index) {
                return Some(offset + index - section.files.start);
            }
            offset += section.files.len();
        }
        None
    }

    pub fn file_row(&self, index: usize) -> Option<usize> {
        let mut offset = 0;
        for section in &self.sections {
            if section.shown && !section.collapsed && section.files.contains(&index) {
                return Some(offset + 1 + index - section.files.start);
            }
            offset += section.rows();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(counts: [usize; 3]) -> Vec<GitFileChange> {
        [Group::Staged, Group::Unstaged, Group::Untracked]
            .into_iter()
            .flat_map(|group| {
                (0..counts[group as usize - 1]).map(move |index| GitFileChange {
                    path: format!("file-{group:?}-{index}.rs"),
                    staged: group == Group::Staged,
                    status: if group == Group::Untracked { '?' } else { 'M' },
                    original_path: None,
                })
            })
            .collect()
    }

    fn reference_rows(
        files: &[GitFileChange],
        visibility: Visibility,
        collapsed: [bool; 4],
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        for group in Group::ALL {
            if !visibility.shows(group) {
                continue;
            }
            let group_files: Vec<_> = files
                .iter()
                .enumerate()
                .filter(|(_, file)| Group::of(file) == group)
                .collect();
            if !group_files.is_empty() || group == Group::Unstaged {
                rows.push(Row::Section(group));
            }
            if !collapsed[group as usize] {
                rows.extend(group_files.into_iter().map(|(index, _)| Row::File(index)));
            }
        }
        rows
    }

    #[test]
    fn geometry_matches_visible_rows_for_all_group_filter_and_collapse_combinations() {
        for staged in 0..3 {
            for unstaged in 0..3 {
                for untracked in 0..3 {
                    let files = files([staged, unstaged, untracked]);
                    for show_untracked in [false, true] {
                        let visibility = Visibility {
                            untracked: show_untracked,
                        };
                        let visible_files: Vec<_> = files
                            .iter()
                            .enumerate()
                            .filter(|(_, file)| visibility.shows(Group::of(file)))
                            .map(|(index, _)| index)
                            .collect();
                        for collapse in 0..16 {
                            let collapsed =
                                std::array::from_fn(|index| collapse & (1 << index) != 0);
                            let expected = reference_rows(&files, visibility, collapsed);
                            let layout = Layout::new(&files, visibility, collapsed);
                            assert_eq!(layout.row_count(), expected.len());
                            assert_eq!(layout.file_count(), visible_files.len());
                            for offset in 0..expected.len() + 3 {
                                assert_eq!(
                                    layout.rows(offset).collect::<Vec<_>>(),
                                    expected.iter().copied().skip(offset).collect::<Vec<_>>()
                                );
                            }
                            for height in 1..12 {
                                assert_eq!(
                                    layout.max_scroll(height),
                                    expected.len().saturating_sub(height)
                                );
                            }
                            assert_eq!(layout.max_scroll(0), 0);
                            for index in 0..files.len() + 1 {
                                assert_eq!(
                                    layout.file_ordinal(index),
                                    visible_files.iter().position(|&file| file == index)
                                );
                                assert_eq!(
                                    layout.file_row(index),
                                    expected.iter().position(|row| *row == Row::File(index))
                                );
                            }
                            for ordinal in 0..visible_files.len() + 1 {
                                assert_eq!(
                                    layout.file_at(ordinal),
                                    visible_files.get(ordinal).copied()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hidden_and_collapsed_large_groups_jump_directly_to_the_viewport() {
        let files = files([50_000, 50_000, 50_000]);
        let layout = Layout::new(
            &files,
            Visibility { untracked: false },
            [false, true, false, false],
        );
        assert_eq!(
            layout.rows(0).take(3).collect::<Vec<_>>(),
            vec![
                Row::Section(Group::Staged),
                Row::Section(Group::Unstaged),
                Row::File(50_000)
            ]
        );
        assert_eq!(
            layout.rows(49_990).take(3).collect::<Vec<_>>(),
            vec![Row::File(99_988), Row::File(99_989), Row::File(99_990)]
        );
        assert_eq!(layout.max_scroll(20), 49_982);
    }

    #[test]
    fn preferences_only_filter_untracked_files_and_default_to_showing_them() {
        assert_eq!(
            serde_json::from_str::<Visibility>("{}").unwrap(),
            Visibility::default()
        );
        assert_eq!(
            serde_json::from_str::<Visibility>(r#"{"untracked":false}"#).unwrap(),
            Visibility { untracked: false }
        );
        for untracked in [false, true] {
            let visibility: Visibility = serde_json::from_value(serde_json::json!({
                "staged": false, "unstaged": false, "untracked": untracked,
            }))
            .unwrap();
            assert!(visibility.shows(Group::Staged));
            assert!(visibility.shows(Group::Unstaged));
            assert_eq!(visibility.shows(Group::Untracked), untracked);
            assert_eq!(
                serde_json::to_value(visibility).unwrap(),
                serde_json::json!({"untracked": untracked})
            );
        }
    }

    #[test]
    #[ignore = "manual performance measurement"]
    fn benchmark_source_control_scroll_geometry() {
        use std::{hint::black_box, time::Instant};
        let files = files([50_000, 50_000, 50_000]);
        let visibility = Visibility { untracked: false };
        let start = Instant::now();
        for _ in 0..100_000 {
            let layout = Layout::new(
                black_box(&files),
                black_box(visibility),
                [false, true, false, false],
            );
            black_box(layout.max_scroll(20));
            black_box(layout.rows(49_000).take(20).count());
        }
        eprintln!(
            "source_control_geometry files=150000 filtered+collapsed+scrolled {:?}/iteration",
            start.elapsed() / 100_000
        );
    }
}
