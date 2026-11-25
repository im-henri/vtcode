//! Normal and Visual mode command grammar:
//! `["x][count]{operator}[count]{motion|text-object}` and friends.

use super::keys::{K, Key};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Reindent,
    Filter,
    Tilde,
    Lower,
    Upper,
    Rot13,
    Format,
    FormatKeep,
    Fold,
    Comment,
}

impl Op {
    pub(crate) fn changes_text(self) -> bool {
        !matches!(self, Op::Yank | Op::Fold)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// `<BS>`/`<C-h>` and `<Space>`: wrap across lines (whichwrap=b,s).
    BackChar,
    FwdChar,
    WordFwd(bool),
    WordBack(bool),
    WordEnd(bool),
    WordEndBack(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    LastNonBlank,
    Column,
    GotoLine,
    GotoFirstLine,
    LineUpFirstNonBlank,
    LineDownFirstNonBlank,
    CurrentLineFirstNonBlank,
    ScreenTop,
    ScreenMiddle,
    ScreenBottom,
    ParaFwd,
    ParaBack,
    SentFwd,
    SentBack,
    Percent,
    SearchNext,
    SearchPrev,
    StarFwd(bool),
    StarBack(bool),
    Find {
        forward: bool,
        till: bool,
        c: char,
    },
    RepeatFind,
    RepeatFindRev,
    Mark {
        c: char,
        line: bool,
    },
    DisplayDown,
    DisplayUp,
    DisplayLineStart,
    DisplayFirstNonBlank,
    DisplayLineEnd,
    DisplayMiddle,
    LineMiddle,
    ByteOffset,
    SectionFwd(char),
    SectionBack(char),
    Unmatched(char),
    Method {
        forward: bool,
        open: bool,
    },
    /// `/` or `?`: completed by the command line.
    Search(bool),
    /// `gn`/`gN`: the next match as an area.
    MatchArea(bool),
}

impl Motion {
    pub(crate) fn is_jump(self) -> bool {
        matches!(
            self,
            Motion::GotoLine
                | Motion::GotoFirstLine
                | Motion::ScreenTop
                | Motion::ScreenMiddle
                | Motion::ScreenBottom
                | Motion::ParaFwd
                | Motion::ParaBack
                | Motion::SentFwd
                | Motion::SentBack
                | Motion::Percent
                | Motion::SearchNext
                | Motion::SearchPrev
                | Motion::StarFwd(_)
                | Motion::StarBack(_)
                | Motion::Mark { .. }
                | Motion::Search(_)
                | Motion::SectionFwd(_)
                | Motion::SectionBack(_)
                | Motion::ByteOffset
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Obj {
    Word(bool),
    Sentence,
    Paragraph,
    Block(char, char),
    Quote(char),
    Tag,
    /// `ie`/`ae`: the whole buffer (common plugin object).
    Entire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Force {
    Char,
    Line,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Motion(Motion),
    Obj {
        obj: Obj,
        inner: bool,
    },
    /// `dd`, `cc`, `yy`, `>>`, `gUU`...
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InsertKind {
    /// `i`
    Before,
    /// `a`
    After,
    /// `I`
    LineStart,
    /// `gI`
    Column0,
    /// `A`
    LineEnd,
    /// `o`
    OpenBelow,
    /// `O`
    OpenAbove,
    /// `gi`
    LastInsert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scroll {
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
    LineDown,
    LineUp,
    /// `zt`, `z<CR>`
    Top(bool),
    /// `zz`, `z.`
    Center(bool),
    /// `zb`, `z-`
    Bottom(bool),
    Left(bool),
    Right(bool),
    HalfLeft,
    HalfRight,
    CursorLeft,
    CursorRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FoldCmd {
    Open,
    OpenAll,
    Close,
    CloseAll,
    Toggle,
    ToggleAll,
    OpenCursor,
    OpenEverything,
    CloseEverything,
    Delete,
    DeleteAll,
    Next,
    Prev,
    Recompute,
    ToggleEnable,
    StartNext,
    StartPrev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Window {
    Left,
    Down,
    Up,
    Right,
    Next,
    Prev,
    Split,
    VSplit,
    Close,
    Only,
    New,
    Top,
    Bottom,
    Equalize,
    Rotate,
    Exchange,
    MaxHeight,
    MaxWidth,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    ToTab,
    Last,
    /// `<C-w>+` `<C-w>-` `<C-w>>` `<C-w><`.
    Taller,
    Shorter,
    Wider,
    Narrower,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Simple {
    Insert(InsertKind),
    Visual(Force),
    Reselect,
    DeleteChar,
    DeleteCharBefore,
    Substitute,
    SubstituteLine,
    Replace(char),
    ReplaceMode,
    VirtualReplace,
    ToggleCase,
    Join(bool),
    Put {
        before: bool,
        cursor_after: bool,
        indent: bool,
    },
    Undo,
    Redo,
    UndoLine,
    Repeat,
    DeleteToEnd,
    ChangeToEnd,
    YankLine,
    SetMark(char),
    Record(char),
    Play(char),
    WriteQuit,
    QuitNoWrite,
    Increment(bool),
    /// Visual `g<C-a>`/`g<C-x>`: each line by one more than the last.
    IncrementSeq(bool),
    Scroll(Scroll),
    Fold(FoldCmd),
    NextTab,
    PrevTab,
    Definition,
    Declaration,
    TypeDefinition,
    References,
    Hover,
    CodeAction,
    GotoFile,
    OpenUrl,
    JumpOlder,
    JumpNewer,
    Alternate,
    FileInfo,
    CharInfo,
    ChangeOlder,
    ChangeNewer,
    RepeatSub,
    RepeatSubAll,
    CmdLine,
    CmdWindow,
    Window(Window),
    Redraw,
    Cancel,
    NextDiagnostic,
    PrevDiagnostic,
    NextChange,
    PrevChange,
    /// `[ <Space>` / `] <Space>`: blank line above/below (unimpaired).
    BlankLine(bool),
    /// Comment toggle on the current line (`gcc` is handled as an operator).
    WordCount,
    // Visual-only commands
    VisualSwap,
    VisualSwapCorner,
    VisualBlockInsert(bool),
    VisualSearch(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Motion(Motion),
    Obj {
        obj: Obj,
        inner: bool,
    },
    Op {
        op: Op,
        force: Option<Force>,
        target: Target,
        count2: Option<usize>,
    },
    /// An operator in Visual mode (applies to the selection).
    VisualOp(Op),
    Simple(Simple),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cmd {
    pub(crate) reg: Option<char>,
    pub(crate) count: Option<usize>,
    pub(crate) action: Action,
}

impl Cmd {
    pub(crate) fn count1(&self) -> usize {
        let c2 = match &self.action {
            Action::Op { count2, .. } => *count2,
            _ => None,
        };
        match (self.count, c2) {
            (None, None) => 1,
            (a, b) => a.unwrap_or(1).saturating_mul(b.unwrap_or(1)),
        }
    }
    pub(crate) fn has_count(&self) -> bool {
        self.count.is_some()
            || matches!(
                &self.action,
                Action::Op {
                    count2: Some(_),
                    ..
                }
            )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    Need,
    Bad,
    Done(Cmd),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctx {
    Normal,
    Visual,
}

struct P<'a> {
    keys: &'a [Key],
    i: usize,
}

enum R<T> {
    Need,
    Bad,
    Ok(T),
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<Key> {
        self.keys.get(self.i).copied()
    }
    fn next(&mut self) -> Option<Key> {
        let k = self.peek();
        if k.is_some() {
            self.i += 1;
        }
        k
    }
    fn count(&mut self) -> Option<usize> {
        let mut n: Option<usize> = None;
        while let Some(c) = self.peek().and_then(|k| k.char()) {
            if c.is_ascii_digit() && (c != '0' || n.is_some()) {
                n = Some(
                    n.unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(c as usize - '0' as usize)
                        .min(999_999_999),
                );
                self.i += 1;
            } else {
                break;
            }
        }
        n
    }
    /// A literal character argument (`f{c}`, `r{c}`, `m{c}`).
    fn arg(&mut self) -> R<char> {
        match self.next() {
            None => R::Need,
            Some(k) if k.is_escape() => R::Bad,
            Some(k) => match (k.k, k.ctrl) {
                (K::Char(c), false) => R::Ok(c),
                (K::Tab, _) => R::Ok('\t'),
                (K::Enter, _) => R::Ok('\r'),
                (K::Char(c), true) if c.is_ascii_alphabetic() || "[\\]^_".contains(c) => {
                    R::Ok(((c.to_ascii_uppercase() as u8) & 0x1f) as char)
                }
                _ => R::Bad,
            },
        }
    }
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            R::Ok(v) => v,
            R::Need => return R::Need,
            R::Bad => return R::Bad,
        }
    };
}

fn motion(p: &mut P<'_>, operator: bool) -> R<Motion> {
    let Some(k) = p.next() else {
        return R::Need;
    };
    use Motion::*;
    if k.ctrl && !k.alt {
        return match k.k {
            K::Char('h') => R::Ok(BackChar),
            // <C-n>/<C-p> stay with the workbench (New File, Quick Open).
            K::Char('j') => R::Ok(Down),
            K::Char('m') => R::Ok(LineDownFirstNonBlank),
            K::Left => R::Ok(WordBack(true)),
            K::Right => R::Ok(WordFwd(true)),
            K::Home => R::Ok(GotoFirstLine),
            K::End => R::Ok(GotoLine),
            _ => R::Bad,
        };
    }
    if k.alt {
        return R::Bad;
    }
    if k.shift {
        return match k.k {
            K::Left => R::Ok(WordBack(false)),
            K::Right => R::Ok(WordFwd(false)),
            K::Up => R::Ok(Up),
            K::Down => R::Ok(Down),
            _ => R::Bad,
        };
    }
    let c = match k.k {
        K::Char(c) => c,
        K::Left => return R::Ok(Left),
        K::Right => return R::Ok(Right),
        K::Up => return R::Ok(Up),
        K::Down => return R::Ok(Down),
        K::Home => return R::Ok(LineStart),
        K::End => return R::Ok(LineEnd),
        K::Backspace => return R::Ok(BackChar),
        K::Enter => return R::Ok(LineDownFirstNonBlank),
        _ => return R::Bad,
    };
    R::Ok(match c {
        'h' => Left,
        'l' => Right,
        'j' => Down,
        'k' => Up,
        ' ' => FwdChar,
        'w' => WordFwd(false),
        'W' => WordFwd(true),
        'b' => WordBack(false),
        'B' => WordBack(true),
        'e' => WordEnd(false),
        'E' => WordEnd(true),
        '0' => LineStart,
        '^' => FirstNonBlank,
        '$' => LineEnd,
        '|' => Column,
        'G' => GotoLine,
        '-' => LineUpFirstNonBlank,
        '+' => LineDownFirstNonBlank,
        '_' => CurrentLineFirstNonBlank,
        'H' => ScreenTop,
        'M' => ScreenMiddle,
        'L' => ScreenBottom,
        '}' => ParaFwd,
        '{' => ParaBack,
        ')' => SentFwd,
        '(' => SentBack,
        '%' => Percent,
        'n' => SearchNext,
        'N' => SearchPrev,
        '*' => StarFwd(true),
        '#' => StarBack(true),
        ';' => RepeatFind,
        ',' => RepeatFindRev,
        '/' => Search(true),
        '?' => Search(false),
        'f' | 'F' | 't' | 'T' => {
            let ch = tri!(p.arg());
            Find {
                forward: c == 'f' || c == 't',
                till: c == 't' || c == 'T',
                c: ch,
            }
        }
        '`' | '\'' => {
            let ch = tri!(p.arg());
            Mark {
                c: ch,
                line: c == '\'',
            }
        }
        'g' => {
            let Some(k2) = p.next() else {
                return R::Need;
            };
            match k2.char() {
                Some('g') => GotoFirstLine,
                Some('e') => WordEndBack(false),
                Some('E') => WordEndBack(true),
                Some('j') => DisplayDown,
                Some('k') => DisplayUp,
                Some('0') => DisplayLineStart,
                Some('^') => DisplayFirstNonBlank,
                Some('$') => DisplayLineEnd,
                Some('m') => DisplayMiddle,
                Some('M') => LineMiddle,
                Some('_') => LastNonBlank,
                Some('o') => ByteOffset,
                Some('*') => StarFwd(false),
                Some('#') => StarBack(false),
                Some('n') => MatchArea(true),
                Some('N') => MatchArea(false),
                Some('\'') | Some('`') => {
                    let ch = tri!(p.arg());
                    Mark {
                        c: ch,
                        line: k2.char() == Some('\''),
                    }
                }
                _ => match k2.k {
                    K::Down => DisplayDown,
                    K::Up => DisplayUp,
                    K::Home => DisplayLineStart,
                    K::End => DisplayLineEnd,
                    _ => return R::Bad,
                },
            }
        }
        '[' | ']' => {
            let fwd = c == ']';
            let Some(k2) = p.next() else {
                return R::Need;
            };
            match k2.char() {
                Some('[') => {
                    if fwd {
                        SectionFwd('}')
                    } else {
                        SectionBack('{')
                    }
                }
                Some(']') => {
                    if fwd {
                        SectionFwd('{')
                    } else {
                        SectionBack('}')
                    }
                }
                Some('(') if !fwd => Unmatched('('),
                Some('{') if !fwd => Unmatched('{'),
                Some(')') if fwd => Unmatched(')'),
                Some('}') if fwd => Unmatched('}'),
                Some('m') => Method {
                    forward: fwd,
                    open: true,
                },
                Some('M') => Method {
                    forward: fwd,
                    open: false,
                },
                Some('\'') | Some('`') if !operator => return R::Bad,
                _ => return R::Bad,
            }
        }
        _ => return R::Bad,
    })
}

fn object(p: &mut P<'_>) -> R<(Obj, bool)> {
    let Some(k) = p.next() else {
        return R::Need;
    };
    let inner = match k.char() {
        Some('i') => true,
        Some('a') => false,
        _ => return R::Bad,
    };
    let Some(k2) = p.next() else {
        return R::Need;
    };
    let obj = match k2.char() {
        Some('w') => Obj::Word(false),
        Some('W') => Obj::Word(true),
        Some('s') => Obj::Sentence,
        Some('p') => Obj::Paragraph,
        Some('(' | ')' | 'b') => Obj::Block('(', ')'),
        Some('{' | '}' | 'B') => Obj::Block('{', '}'),
        Some('[' | ']') => Obj::Block('[', ']'),
        Some('<' | '>') => Obj::Block('<', '>'),
        Some(q @ ('"' | '\'' | '`')) => Obj::Quote(q),
        Some('t') => Obj::Tag,
        Some('e') => Obj::Entire,
        _ => return R::Bad,
    };
    R::Ok((obj, inner))
}

/// Peek whether the next keys start a text object (`i`/`a` in operator
/// context are never commands).
fn starts_object(p: &P<'_>) -> bool {
    matches!(p.peek().and_then(|k| k.char()), Some('i' | 'a'))
}

fn operator(p: &mut P<'_>) -> Option<R<Op>> {
    let save = p.i;
    let k = p.peek()?;
    if k.ctrl || k.alt {
        return None;
    }
    let c = k.char()?;
    let op = match c {
        'd' => Op::Delete,
        'c' => Op::Change,
        'y' => Op::Yank,
        '>' => Op::Indent,
        '<' => Op::Outdent,
        '=' => Op::Reindent,
        '!' => Op::Filter,
        'g' => {
            p.i += 1;
            let Some(k2) = p.peek() else {
                p.i = save;
                return Some(R::Need);
            };
            let op = match k2.char() {
                Some('~') => Op::Tilde,
                Some('u') => Op::Lower,
                Some('U') => Op::Upper,
                Some('?') => Op::Rot13,
                Some('q') => Op::Format,
                Some('w') => Op::FormatKeep,
                Some('c') => Op::Comment,
                _ => {
                    p.i = save;
                    return None;
                }
            };
            p.i += 1;
            return Some(R::Ok(op));
        }
        'z' => {
            p.i += 1;
            let Some(k2) = p.peek() else {
                p.i = save;
                return Some(R::Need);
            };
            if k2.char() == Some('f') {
                p.i += 1;
                return Some(R::Ok(Op::Fold));
            }
            p.i = save;
            return None;
        }
        _ => return None,
    };
    p.i += 1;
    Some(R::Ok(op))
}

fn simple(p: &mut P<'_>, ctx: Ctx) -> R<Simple> {
    use Simple::*;
    let Some(k) = p.next() else {
        return R::Need;
    };
    if k.is_escape() || k.is_ctrl('c') {
        return R::Ok(Cancel);
    }
    if k.ctrl && !k.alt {
        let K::Char(c) = k.k else {
            return match k.k {
                K::PageUp => R::Ok(PrevTab),
                K::PageDown => R::Ok(NextTab),
                _ => R::Bad,
            };
        };
        return R::Ok(match c {
            'r' => Redo,
            'a' => Increment(true),
            'x' => Increment(false),
            'd' => Scroll(self::Scroll::HalfDown),
            'u' => Scroll(self::Scroll::HalfUp),
            'f' => Scroll(self::Scroll::PageDown),
            'b' => Scroll(self::Scroll::PageUp),
            'e' => Scroll(self::Scroll::LineDown),
            'y' => Scroll(self::Scroll::LineUp),
            'o' => JumpOlder,
            'i' => JumpNewer,
            ']' => Definition,
            't' => JumpOlder,
            '^' | '6' => Alternate,
            'g' => FileInfo,
            'l' => Redraw,
            // <C-q> is Ctrl+Q, Quit, in the workbench (see `unclaimed`);
            // <C-v> already starts Visual block mode, paste or not.
            'v' => Visual(super::parse::Force::Block),
            'w' => {
                let Some(k2) = p.next() else {
                    return R::Need;
                };
                let c2 = match k2.k {
                    K::Char(c) => c,
                    K::Left => 'h',
                    K::Right => 'l',
                    K::Up => 'k',
                    K::Down => 'j',
                    _ => return R::Bad,
                };
                match window_cmd(c2) {
                    Some(w) => Window(w),
                    None => return R::Bad,
                }
            }
            _ => return R::Bad,
        });
    }
    if k.alt {
        return R::Bad;
    }
    let c = match k.k {
        K::Char(c) => c,
        K::Insert => return R::Ok(Insert(InsertKind::Before)),
        K::Delete => {
            return R::Ok(if ctx == Ctx::Visual {
                Cancel
            } else {
                DeleteChar
            });
        }
        K::Tab => return R::Ok(JumpNewer),
        K::PageDown => return R::Ok(Scroll(self::Scroll::PageDown)),
        K::PageUp => return R::Ok(Scroll(self::Scroll::PageUp)),
        K::F(1) => return R::Bad,
        _ => return R::Bad,
    };
    let visual = ctx == Ctx::Visual;
    R::Ok(match c {
        'i' if !visual => Insert(InsertKind::Before),
        'a' if !visual => Insert(InsertKind::After),
        'I' if visual => VisualBlockInsert(false),
        'A' if visual => VisualBlockInsert(true),
        'I' => Insert(InsertKind::LineStart),
        'A' => Insert(InsertKind::LineEnd),
        'o' if visual => VisualSwap,
        'O' if visual => VisualSwapCorner,
        'o' => Insert(InsertKind::OpenBelow),
        'O' => Insert(InsertKind::OpenAbove),
        'v' => Visual(Force::Char),
        'V' => Visual(Force::Line),
        'x' => DeleteChar,
        'X' => DeleteCharBefore,
        's' => Substitute,
        'S' => SubstituteLine,
        'r' => Replace(tri!(p.arg())),
        'R' => ReplaceMode,
        '~' => ToggleCase,
        'J' => Join(true),
        'p' => Put {
            before: false,
            cursor_after: false,
            indent: false,
        },
        'P' => Put {
            before: true,
            cursor_after: false,
            indent: false,
        },
        'u' => Undo,
        'U' => UndoLine,
        '.' => Repeat,
        'D' => DeleteToEnd,
        'C' => ChangeToEnd,
        'Y' => YankLine,
        'm' => SetMark(tri!(p.arg())),
        'q' => {
            let Some(k2) = p.next() else {
                return R::Need;
            };
            match k2.char() {
                Some(':' | '/' | '?') => CmdWindow,
                Some(r) if r.is_ascii_alphanumeric() || r == '"' => Record(r),
                _ => return R::Bad,
            }
        }
        '@' => {
            let r = tri!(p.arg());
            if r.is_ascii_alphanumeric() || "@:\".*+-=".contains(r) {
                Play(r)
            } else {
                return R::Bad;
            }
        }
        '&' => RepeatSub,
        ':' => CmdLine,
        'K' => Hover,
        'Z' => {
            let Some(k2) = p.next() else {
                return R::Need;
            };
            match k2.char() {
                Some('Z') => WriteQuit,
                Some('Q') => QuitNoWrite,
                _ => return R::Bad,
            }
        }
        'z' => {
            let Some(k2) = p.next() else {
                return R::Need;
            };
            use self::Scroll as S;
            if k2.k == K::Enter {
                return R::Ok(Scroll(S::Top(true)));
            }
            match k2.k {
                K::Left => return R::Ok(Scroll(S::Left(false))),
                K::Right => return R::Ok(Scroll(S::Right(false))),
                _ => {}
            }
            match k2.char() {
                Some('t') => Scroll(S::Top(false)),
                Some('z') => Scroll(S::Center(false)),
                Some('.') => Scroll(S::Center(true)),
                Some('b') => Scroll(S::Bottom(false)),
                Some('-') => Scroll(S::Bottom(true)),
                Some('h') => Scroll(S::Left(false)),
                Some('l') => Scroll(S::Right(false)),
                Some('H') => Scroll(S::HalfLeft),
                Some('L') => Scroll(S::HalfRight),
                Some('s') => Scroll(S::CursorLeft),
                Some('e') => Scroll(S::CursorRight),
                Some('o') => Fold(FoldCmd::Open),
                Some('O') => Fold(FoldCmd::OpenAll),
                Some('c') => Fold(FoldCmd::Close),
                Some('C') => Fold(FoldCmd::CloseAll),
                Some('a') => Fold(FoldCmd::Toggle),
                Some('A') => Fold(FoldCmd::ToggleAll),
                Some('v') => Fold(FoldCmd::OpenCursor),
                Some('R') => Fold(FoldCmd::OpenEverything),
                Some('M') => Fold(FoldCmd::CloseEverything),
                Some('d') => Fold(FoldCmd::Delete),
                Some('E') => Fold(FoldCmd::DeleteAll),
                Some('j') => Fold(FoldCmd::Next),
                Some('k') => Fold(FoldCmd::Prev),
                Some('x' | 'X') => Fold(FoldCmd::Recompute),
                Some('i' | 'n' | 'N') => Fold(FoldCmd::ToggleEnable),
                _ => return R::Bad,
            }
        }
        'g' => {
            let Some(k2) = p.next() else {
                return R::Need;
            };
            if k2.is_ctrl('g') {
                return R::Ok(WordCount);
            }
            if k2.is_ctrl('a') && visual {
                return R::Ok(IncrementSeq(true));
            }
            if k2.is_ctrl('x') && visual {
                return R::Ok(IncrementSeq(false));
            }
            match k2.char() {
                Some('i') => Insert(InsertKind::LastInsert),
                Some('I') => Insert(InsertKind::Column0),
                Some('v') => Reselect,
                Some('J') => Join(false),
                Some('p') => Put {
                    before: false,
                    cursor_after: true,
                    indent: false,
                },
                Some('P') => Put {
                    before: true,
                    cursor_after: true,
                    indent: false,
                },
                Some('t') => NextTab,
                Some('T') => PrevTab,
                Some('d') => Definition,
                Some('D') => Declaration,
                Some('y') => TypeDefinition,
                Some('r') => References,
                Some('R') => VirtualReplace,
                Some('h') => Hover,
                Some('f' | 'F') => GotoFile,
                Some('x') => OpenUrl,
                Some('a') => CharInfo,
                Some(';') => ChangeOlder,
                Some(',') => ChangeNewer,
                Some('&') => RepeatSubAll,
                Some('.') => CodeAction,
                _ => return R::Bad,
            }
        }
        '[' | ']' => {
            let fwd = c == ']';
            let Some(k2) = p.next() else {
                return R::Need;
            };
            match k2.char() {
                Some('p' | 'P') => Put {
                    before: !fwd || k2.char() == Some('P'),
                    cursor_after: false,
                    indent: true,
                },
                Some('d') => {
                    if fwd {
                        NextDiagnostic
                    } else {
                        PrevDiagnostic
                    }
                }
                Some('c') => {
                    if fwd {
                        NextChange
                    } else {
                        PrevChange
                    }
                }
                Some('z') => Fold(if fwd {
                    FoldCmd::StartNext
                } else {
                    FoldCmd::StartPrev
                }),
                Some(' ') => BlankLine(fwd),
                _ => return R::Bad,
            }
        }
        '*' if visual => VisualSearch(true),
        '#' if visual => VisualSearch(false),
        _ => return R::Bad,
    })
}

/// The command after `Ctrl-W`, from the editor or any other pane.
pub(crate) fn window_cmd(c: char) -> Option<Window> {
    use self::Window as W;
    Some(match c {
        'h' => W::Left,
        'j' => W::Down,
        'k' => W::Up,
        'l' => W::Right,
        'w' => W::Next,
        'W' => W::Prev,
        'p' => W::Last,
        's' | 'S' => W::Split,
        'v' => W::VSplit,
        'c' | 'q' => W::Close,
        'o' => W::Only,
        'n' => W::New,
        't' => W::Top,
        'b' => W::Bottom,
        '=' => W::Equalize,
        'r' | 'R' => W::Rotate,
        'x' => W::Exchange,
        '_' => W::MaxHeight,
        '|' => W::MaxWidth,
        'H' => W::MoveLeft,
        'L' => W::MoveRight,
        'K' => W::MoveUp,
        'J' => W::MoveDown,
        'T' => W::ToTab,
        '+' => W::Taller,
        '-' => W::Shorter,
        '>' => W::Wider,
        '<' => W::Narrower,
        _ => return None,
    })
}

/// Parse a complete Normal (or Visual) mode command from `keys`.
pub(crate) fn parse(keys: &[Key], ctx: Ctx) -> Parsed {
    let mut p = P { keys, i: 0 };
    let mut reg = None;
    let mut count = p.count();
    if p.peek().and_then(|k| k.char()) == Some('"') {
        p.i += 1;
        match p.next() {
            None => return Parsed::Need,
            Some(k) => match k.char() {
                Some(r) if r.is_ascii_alphanumeric() || "\"-_*+.:%#/=".contains(r) => reg = Some(r),
                _ => return Parsed::Bad,
            },
        }
        let c2 = p.count();
        count = match (count, c2) {
            (Some(a), Some(b)) => Some(a.saturating_mul(b)),
            (a, b) => a.or(b),
        };
    }
    if p.peek().is_none() {
        return Parsed::Need;
    }
    let done = |action| Parsed::Done(Cmd { reg, count, action });
    let start = p.i;

    if ctx == Ctx::Visual {
        // Text objects extend the selection.
        if starts_object(&p) {
            return match object(&mut p) {
                R::Need => Parsed::Need,
                R::Bad => Parsed::Bad,
                R::Ok((obj, inner)) => done(Action::Obj { obj, inner }),
            };
        }
        if let Some(k) = p.peek() {
            let op = if k.k == K::Delete && !k.ctrl && !k.alt {
                Some(Op::Delete)
            } else {
                match k.char() {
                    Some('d' | 'x') => Some(Op::Delete),
                    Some('y') => Some(Op::Yank),
                    Some('c' | 's') => Some(Op::Change),
                    Some('>') => Some(Op::Indent),
                    Some('<') => Some(Op::Outdent),
                    Some('=') => Some(Op::Reindent),
                    Some('!') => Some(Op::Filter),
                    Some('~') => Some(Op::Tilde),
                    Some('u') => Some(Op::Lower),
                    Some('U') => Some(Op::Upper),
                    _ => None,
                }
            };
            if let Some(op) = op {
                return done(Action::VisualOp(op));
            }
            // Uppercase linewise variants.
            if let Some(c) = k.char() {
                let simple = match c {
                    'X' | 'D' => Some(Simple::DeleteToEnd),
                    'Y' => Some(Simple::YankLine),
                    'C' | 'S' | 'R' => Some(Simple::ChangeToEnd),
                    'J' => Some(Simple::Join(true)),
                    _ => None,
                };
                if let Some(s) = simple {
                    return done(Action::Simple(s));
                }
            }
            if k.char() == Some('g') {
                let save = p.i;
                p.i += 1;
                if p.peek().is_none() {
                    return Parsed::Need;
                }
                match p.peek().and_then(|k| k.char()) {
                    Some('~') => return done(Action::VisualOp(Op::Tilde)),
                    Some('u') => return done(Action::VisualOp(Op::Lower)),
                    Some('U') => return done(Action::VisualOp(Op::Upper)),
                    Some('?') => return done(Action::VisualOp(Op::Rot13)),
                    Some('q') => return done(Action::VisualOp(Op::Format)),
                    Some('w') => return done(Action::VisualOp(Op::FormatKeep)),
                    Some('c') => return done(Action::VisualOp(Op::Comment)),
                    _ => p.i = save,
                }
            }
            if k.char() == Some('z') {
                let save = p.i;
                p.i += 1;
                if p.peek().is_none() {
                    return Parsed::Need;
                }
                match p.peek().and_then(|k| k.char()) {
                    Some('f') => return done(Action::VisualOp(Op::Fold)),
                    _ => p.i = save,
                }
            }
        }
    } else if let Some(op) = operator(&mut p) {
        let op = match op {
            R::Need => return Parsed::Need,
            R::Bad => return Parsed::Bad,
            R::Ok(op) => op,
        };
        let op_keys = &keys[start..p.i];
        let count2 = p.count();
        let mut force = None;
        loop {
            match p.peek() {
                None => return Parsed::Need,
                Some(k) if k.char() == Some('v') => {
                    force = Some(Force::Char);
                    p.i += 1;
                }
                Some(k) if k.char() == Some('V') => {
                    force = Some(Force::Line);
                    p.i += 1;
                }
                Some(k) if k.is_ctrl('v') => {
                    force = Some(Force::Block);
                    p.i += 1;
                }
                _ => break,
            }
        }
        let rest = &keys[p.i..];
        // `dd`, `g~~`, `gUU`, `gUgU`, `gcc`.
        if rest.is_empty() {
            return Parsed::Need;
        }
        let doubled = if op == Op::Fold {
            false
        } else if op_keys.len() == 2 {
            if rest[0] == op_keys[1] {
                p.i += 1;
                true
            } else if rest[0] == op_keys[0] {
                if rest.len() < 2 {
                    return Parsed::Need;
                }
                if rest[1] == op_keys[1] {
                    p.i += 2;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else if rest[0] == op_keys[0] {
            p.i += 1;
            true
        } else {
            false
        };
        if doubled {
            return done(Action::Op {
                op,
                force,
                target: Target::Lines,
                count2,
            });
        }
        if starts_object(&p) {
            return match object(&mut p) {
                R::Need => Parsed::Need,
                R::Bad => Parsed::Bad,
                R::Ok((obj, inner)) => done(Action::Op {
                    op,
                    force,
                    target: Target::Obj { obj, inner },
                    count2,
                }),
            };
        }
        return match motion(&mut p, true) {
            R::Need => Parsed::Need,
            R::Bad => Parsed::Bad,
            R::Ok(m) => done(Action::Op {
                op,
                force,
                target: Target::Motion(m),
                count2,
            }),
        };
    }

    // Plain motions.
    let save = p.i;
    match motion(&mut p, false) {
        R::Ok(m) => return done(Action::Motion(m)),
        R::Need => {
            // `g`, `z`, `[`, `]`, `Z` prefixes are shared with commands.
            let mut q = P { keys, i: save };
            if let R::Need = simple(&mut q, ctx) {
                return Parsed::Need;
            }
            let mut q = P { keys, i: save };
            if let R::Ok(s) = simple(&mut q, ctx) {
                return done(Action::Simple(s));
            }
            return Parsed::Need;
        }
        R::Bad => {}
    }
    p.i = save;
    match simple(&mut p, ctx) {
        R::Ok(s) => done(Action::Simple(s)),
        R::Need => Parsed::Need,
        R::Bad => Parsed::Bad,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::keys::parse as keys;

    fn p(s: &str) -> Parsed {
        parse(&keys(s, &[]), Ctx::Normal)
    }
    fn cmd(s: &str) -> Cmd {
        match p(s) {
            Parsed::Done(c) => c,
            other => panic!("{s}: {other:?}"),
        }
    }

    #[test]
    fn operators_counts_and_registers_compose() {
        let c = cmd("\"a2d3w");
        assert_eq!(c.reg, Some('a'));
        assert_eq!(c.count1(), 6);
        assert_eq!(
            c.action,
            Action::Op {
                op: Op::Delete,
                force: None,
                target: Target::Motion(Motion::WordFwd(false)),
                count2: Some(3)
            }
        );
        assert!(matches!(
            cmd("dd").action,
            Action::Op {
                target: Target::Lines,
                ..
            }
        ));
        assert!(matches!(
            cmd("gUU").action,
            Action::Op {
                op: Op::Upper,
                target: Target::Lines,
                ..
            }
        ));
        assert!(matches!(
            cmd("gUgU").action,
            Action::Op {
                op: Op::Upper,
                target: Target::Lines,
                ..
            }
        ));
        assert!(matches!(
            cmd("g~~").action,
            Action::Op {
                op: Op::Tilde,
                target: Target::Lines,
                ..
            }
        ));
        assert!(matches!(
            cmd("gcc").action,
            Action::Op {
                op: Op::Comment,
                target: Target::Lines,
                ..
            }
        ));
        assert!(matches!(
            cmd("ci\"").action,
            Action::Op {
                op: Op::Change,
                target: Target::Obj {
                    obj: Obj::Quote('"'),
                    inner: true
                },
                ..
            }
        ));
        assert!(matches!(
            cmd("dvj").action,
            Action::Op {
                force: Some(Force::Char),
                ..
            }
        ));
        assert!(matches!(
            cmd("dt)").action,
            Action::Op {
                target: Target::Motion(Motion::Find {
                    forward: true,
                    till: true,
                    c: ')'
                }),
                ..
            }
        ));
        assert!(matches!(
            cmd("d/").action,
            Action::Op {
                target: Target::Motion(Motion::Search(true)),
                ..
            }
        ));
        assert!(matches!(
            cmd("zfap").action,
            Action::Op { op: Op::Fold, .. }
        ));
    }

    #[test]
    fn prefixes_wait_and_garbage_is_rejected() {
        for s in [
            "", "\"", "2", "d", "d2", "g", "gU", "f", "di", "z", "[", "Z", "<C-w>", "q", "m",
            "\"a", "\"a3",
        ] {
            assert_eq!(p(s), Parsed::Need, "{s:?}");
        }
        for s in ["dZ", "Q", "\"!", "d<C-x>", "gQ"] {
            assert_eq!(p(s), Parsed::Bad, "{s:?}");
        }
        assert_eq!(cmd("0").action, Action::Motion(Motion::LineStart));
        assert_eq!(cmd("10G").count, Some(10));
        assert_eq!(cmd("gg").action, Action::Motion(Motion::GotoFirstLine));
        assert_eq!(cmd("gJ").action, Action::Simple(Simple::Join(false)));
        assert_eq!(
            cmd("zz").action,
            Action::Simple(Simple::Scroll(Scroll::Center(false)))
        );
        assert_eq!(
            cmd("<C-w>l").action,
            Action::Simple(Simple::Window(Window::Right))
        );
        assert_eq!(cmd("ra").action, Action::Simple(Simple::Replace('a')));
        assert_eq!(cmd("@@").action, Action::Simple(Simple::Play('@')));
    }

    #[test]
    fn visual_context_reads_operators_and_objects() {
        let v = |s: &str| match parse(&keys(s, &[]), Ctx::Visual) {
            Parsed::Done(c) => c.action,
            o => panic!("{s}: {o:?}"),
        };
        assert_eq!(v("d"), Action::VisualOp(Op::Delete));
        assert_eq!(
            v("iw"),
            Action::Obj {
                obj: Obj::Word(false),
                inner: true
            }
        );
        assert_eq!(v("o"), Action::Simple(Simple::VisualSwap));
        assert_eq!(v("gU"), Action::VisualOp(Op::Upper));
        assert_eq!(v("I"), Action::Simple(Simple::VisualBlockInsert(false)));
        assert_eq!(v("j"), Action::Motion(Motion::Down));
    }
}
