use serde::Deserialize;
use vtcode_extension_api::editor::DocumentInfo;

pub trait Backend {
    fn info(&mut self, id: u32) -> Result<DocumentInfo, String>;
    fn read(&mut self, id: u32, offset: u64, count: usize) -> Result<Vec<u8>, String>;
    fn changed(&mut self, _id: u32, _offset: u64, count: usize) -> Result<Vec<u8>, String> {
        Ok(vec![0; count])
    }
    fn edit(&mut self, id: u32, offset: u64, delete: u64, bytes: &[u8]) -> Result<(), String>;
    fn action(&mut self, id: u32, action: u32) -> Result<bool, String>;
    fn clipboard(&mut self) -> Result<String, String>;
    fn copy(&mut self, text: &str) -> Result<(), String>;
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub width: u16,
    #[serde(default)]
    pub height: u16,
    #[serde(default)]
    pub view_column: u16,
    #[serde(default)]
    pub view_row: u16,
    #[serde(default)]
    pub direction: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Hex,
    Text,
}

#[derive(Clone, Debug)]
pub enum Prompt {
    Goto(String),
    Find {
        query: String,
        replacement: String,
        hex: bool,
        replace: bool,
        field: usize,
    },
}

pub struct HexEditor {
    pub document: u32,
    pub cursor: u64,
    pub anchor: Option<u64>,
    pub top: u64,
    pub pane: Pane,
    pub insert: bool,
    pub little_endian: bool,
    pub inspector: bool,
    pub low_nibble: bool,
    pub width: u16,
    pub height: u16,
    pub prompt: Option<Prompt>,
    pub status: String,
    pub error: bool,
    pub dragging: bool,
    pub reveal_cursor: bool,
    pub toolbar: Vec<(usize, usize, &'static str)>,
    pub search: Option<crate::search::SearchTask>,
    pub last_find: Option<(String, bool)>,
    pub prompt_cursor: usize,
    pub prompt_anchor: Option<usize>,
}

#[derive(Clone, Copy)]
pub struct Layout {
    pub bpr: usize,
    pub offset_width: usize,
    pub hex_start: usize,
    pub ascii_start: usize,
    pub data_start: usize,
    pub rows: usize,
    pub inspector_start: Option<usize>,
    pub inspector_top: Option<usize>,
}

impl HexEditor {
    pub fn new(document: u32) -> Self {
        Self {
            document,
            cursor: 0,
            anchor: None,
            top: 0,
            pane: Pane::Hex,
            insert: false,
            little_endian: true,
            inspector: true,
            low_nibble: false,
            width: 100,
            height: 28,
            prompt: None,
            status: String::new(),
            error: false,
            dragging: false,
            reveal_cursor: true,
            toolbar: Vec::new(),
            search: None,
            last_find: None,
            prompt_cursor: 0,
            prompt_anchor: None,
        }
    }

    pub fn layout(&self, size: u64) -> Layout {
        let width = self.width as usize;
        let offset_width = if size > u32::MAX as u64 { 16 } else { 8 };
        let inspector_start = (self.inspector && width >= 114).then_some(width.saturating_sub(32));
        let inspector_top =
            (self.inspector && inspector_start.is_none() && width >= 40 && self.height >= 18)
                .then_some((self.height as usize).saturating_sub(9));
        let available = inspector_start.unwrap_or(width);
        let bpr = [16, 8, 4, 1]
            .into_iter()
            .find(|n| offset_width + 5 + n * 4 + (n - 1) / 8 <= available)
            .unwrap_or(1);
        let hex_start = offset_width + 2;
        let ascii_start = hex_start + bpr * 3 + (bpr - 1) / 8 + 2;
        let data_start = 2
            + usize::from(self.prompt.is_some())
            + usize::from(matches!(
                self.prompt,
                Some(Prompt::Find { replace: true, .. })
            ));
        let rows = inspector_top
            .unwrap_or(self.height as usize - usize::from(self.height > 0))
            .saturating_sub(data_start)
            .max(1);
        Layout {
            bpr,
            offset_width,
            hex_start,
            ascii_start,
            data_start,
            rows,
            inspector_start,
            inspector_top,
        }
    }

    pub fn selection(&self, size: u64) -> (u64, u64) {
        match self.anchor {
            Some(anchor) if anchor != self.cursor => (
                anchor.min(self.cursor).min(size),
                anchor.max(self.cursor).min(size),
            ),
            _ => (
                self.cursor.min(size),
                self.cursor.saturating_add(1).min(size),
            ),
        }
    }

    fn move_to(&mut self, offset: u64, shift: bool, size: u64) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = offset.min(size);
        self.low_nibble = false;
    }

    pub fn event(&mut self, event: Event, backend: &mut impl Backend) {
        self.error = false;
        self.reveal_cursor = event.direction == 0 && event.kind != "resize";
        if event.width > 0 {
            self.width = event.width;
        }
        if event.height > 0 {
            self.height = event.height;
        }
        let result = self.handle(event, backend);
        if let Err(error) = result {
            self.status = error;
            self.error = true;
        }
    }

    fn handle(&mut self, event: Event, backend: &mut impl Backend) -> Result<(), String> {
        let info = backend.info(self.document)?;
        let layout = self.layout(info.size);
        self.cursor = self.cursor.min(info.size);
        if event.kind == "resize" {
            return Ok(());
        }
        if self.search.is_some() && event.code == "escape" {
            self.cancel_search(backend)?;
            self.status = "Operation cancelled".into();
            return Ok(());
        }
        if self.search.is_some() && !matches!(event.code.as_str(), "" | "other") {
            self.cancel_search(backend)?;
        }
        if event.direction != 0 {
            let step = (layout.bpr * 3) as u64;
            self.top = if event.direction < 0 {
                self.top.saturating_sub(step)
            } else {
                self.top
                    .saturating_add(step)
                    .min(info.size / layout.bpr as u64 * layout.bpr as u64)
            };
            return Ok(());
        }
        if matches!(event.kind.as_str(), "down" | "up" | "drag") {
            return self.pointer(event, &info, layout, backend);
        }
        if event.code == "command"
            && !(self.prompt.is_some()
                && matches!(event.text.as_str(), "paste" | "copy" | "cut" | "select_all"))
        {
            return self.command(&event.text, backend, &info);
        }
        if self.prompt.is_some() {
            return self.prompt_key(event, backend, &info);
        }
        if event.code == "paste" {
            let bytes = if self.pane == Pane::Hex {
                parse_hex(&event.text)?
            } else {
                event.text.into_bytes()
            };
            return self.paste_bytes(&bytes, backend, &info);
        }
        if event.code == "command" {
            return self.command(&event.text, backend, &info);
        }
        if event.control {
            if matches!(event.code.as_str(), "home" | "end") {
                self.move_to(
                    if event.code == "home" { 0 } else { info.size },
                    event.shift,
                    info.size,
                );
                return Ok(());
            }
            let command = match event.text.to_ascii_lowercase().as_str() {
                "s" => "save",
                "z" if event.shift => "redo",
                "z" => "undo",
                "y" => "redo",
                "a" => "select_all",
                "c" => "copy",
                "x" => "cut",
                "v" => "paste",
                "f" => "find",
                "h" => "find_replace",
                "g" => "goto_line",
                _ => "",
            };
            return self.command(command, backend, &info);
        }
        if event.alt {
            match event.text.to_ascii_lowercase().as_str() {
                "i" => self.inspector = !self.inspector,
                "e" => self.little_endian = !self.little_endian,
                _ => {}
            }
            return Ok(());
        }
        match event.code.as_str() {
            "left" => self.move_to(self.cursor.saturating_sub(1), event.shift, info.size),
            "right" => self.move_to(self.cursor.saturating_add(1), event.shift, info.size),
            "up" => self.move_to(
                self.cursor.saturating_sub(layout.bpr as u64),
                event.shift,
                info.size,
            ),
            "down" => self.move_to(
                self.cursor.saturating_add(layout.bpr as u64),
                event.shift,
                info.size,
            ),
            "pageUp" => self.move_to(
                self.cursor
                    .saturating_sub((layout.bpr * layout.rows) as u64),
                event.shift,
                info.size,
            ),
            "pageDown" => self.move_to(
                self.cursor
                    .saturating_add((layout.bpr * layout.rows) as u64),
                event.shift,
                info.size,
            ),
            "home" => self.move_to(
                self.cursor / layout.bpr as u64 * layout.bpr as u64,
                event.shift,
                info.size,
            ),
            "end" => self.move_to(
                (self.cursor / layout.bpr as u64 + 1) * layout.bpr as u64,
                event.shift,
                info.size,
            ),
            "tab" | "backTab" => {
                self.pane = if self.pane == Pane::Hex {
                    Pane::Text
                } else {
                    Pane::Hex
                };
                self.low_nibble = false;
            }
            "insert" => self.insert = !self.insert,
            "escape" => {
                self.anchor = None;
                self.status.clear();
            }
            "delete" | "backspace" => {
                let (mut start, end) = self.selection(info.size);
                let mut delete = end - start;
                if event.code == "backspace" && self.anchor.is_none() {
                    if self.cursor == 0 {
                        return Ok(());
                    }
                    start = self.cursor - 1;
                    delete = 1;
                }
                backend.edit(self.document, start, delete, &[])?;
                self.move_to(start, false, info.size - delete);
            }
            "char" if self.pane == Pane::Hex => {
                let Some(digit) = event.text.chars().next().and_then(|ch| ch.to_digit(16)) else {
                    return Err("Type 0–9 or A–F in the hex grid; Tab switches to text".into());
                };
                let (start, end) = self.selection(info.size);
                let previous = backend
                    .read(self.document, start, 1)?
                    .first()
                    .copied()
                    .unwrap_or(0);
                let value = if self.low_nibble {
                    (previous & 0xf0) | digit as u8
                } else {
                    (digit as u8) << 4 | previous & 0x0f
                };
                let delete = if self.low_nibble {
                    u64::from(self.cursor < info.size)
                } else if self.anchor.is_some() {
                    end - start
                } else if self.insert {
                    0
                } else {
                    u64::from(self.cursor < info.size)
                };
                if self.low_nibble {
                    backend.action(self.document, 5)?;
                }
                backend.edit(self.document, start, delete, &[value])?;
                self.cursor = start;
                self.anchor = None;
                if self.low_nibble {
                    self.cursor += 1;
                }
                self.low_nibble = !self.low_nibble;
                self.status.clear();
            }
            "char" => {
                self.paste_bytes(event.text.as_bytes(), backend, &info)?;
                self.status.clear();
            }
            _ => {}
        }
        Ok(())
    }

    fn command(
        &mut self,
        command: &str,
        backend: &mut impl Backend,
        info: &DocumentInfo,
    ) -> Result<(), String> {
        match command {
            "save" => {
                backend.action(self.document, 3)?;
                self.status = "Saved".into();
            }
            "undo" | "redo" => {
                backend.action(self.document, if command == "undo" { 1 } else { 2 })?;
                self.anchor = None;
                self.low_nibble = false;
                self.status.clear();
            }
            "select_all" => {
                self.anchor = Some(0);
                self.cursor = info.size;
            }
            "find" | "find_replace" => {
                let (query, hex) = self
                    .last_find
                    .clone()
                    .unwrap_or((String::new(), self.pane == Pane::Hex));
                self.prompt_cursor = query.len();
                self.prompt_anchor = (!query.is_empty()).then_some(0);
                self.prompt = Some(Prompt::Find {
                    query,
                    replacement: String::new(),
                    hex,
                    replace: command == "find_replace",
                    field: 0,
                });
            }
            "find_next" | "find_prev" => {
                if self.prompt.is_none() {
                    let Some((query, hex)) = self.last_find.clone() else {
                        return self.command("find", backend, info);
                    };
                    self.prompt = Some(Prompt::Find {
                        query,
                        replacement: String::new(),
                        hex,
                        replace: false,
                        field: 0,
                    });
                }
                self.prompt_key(
                    Event {
                        code: "enter".into(),
                        shift: command == "find_prev",
                        ..Default::default()
                    },
                    backend,
                    info,
                )?;
            }
            "goto_line" => {
                self.prompt_cursor = 0;
                self.prompt_anchor = None;
                self.prompt = Some(Prompt::Goto(String::new()));
            }
            "copy" | "cut" => {
                let (start, end) = self.selection(info.size);
                if end - start > 1024 * 1024 {
                    return Err("Select at most 1 MiB to copy".into());
                }
                let bytes = backend.read(self.document, start, (end - start) as usize)?;
                let text = if self.pane == Pane::Hex {
                    hex_text(&bytes)
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                backend.copy(&text)?;
                self.status = format!("Copied {} bytes", bytes.len());
                if command == "cut" {
                    backend.edit(self.document, start, end - start, &[])?;
                    self.move_to(start, false, info.size - (end - start));
                }
            }
            "paste" => {
                let text = backend.clipboard()?;
                let bytes = if self.pane == Pane::Hex {
                    parse_hex(&text)?
                } else {
                    text.into_bytes()
                };
                self.paste_bytes(&bytes, backend, info)?;
            }
            "inspector" => self.inspector = !self.inspector,
            "endian" => self.little_endian = !self.little_endian,
            "mode" => self.insert = !self.insert,
            _ => {}
        }
        Ok(())
    }

    fn paste_bytes(
        &mut self,
        bytes: &[u8],
        backend: &mut impl Backend,
        info: &DocumentInfo,
    ) -> Result<(), String> {
        let (start, end) = self.selection(info.size);
        let deleted = if self.anchor.is_some() {
            end - start
        } else if self.insert {
            0
        } else {
            (bytes.len() as u64).min(info.size - start)
        };
        backend.edit(self.document, start, deleted, bytes)?;
        self.move_to(
            start + bytes.len() as u64,
            false,
            info.size - deleted + bytes.len() as u64,
        );
        Ok(())
    }

    fn pointer(
        &mut self,
        event: Event,
        info: &DocumentInfo,
        layout: Layout,
        backend: &mut impl Backend,
    ) -> Result<(), String> {
        if event.kind == "up" {
            self.dragging = false;
            return Ok(());
        }
        let x = event.view_column as usize;
        let y = event.view_row as usize;
        if event.kind == "down"
            && y == 0
            && let Some((_, _, command)) = self
                .toolbar
                .iter()
                .find(|(start, end, _)| x >= *start && x < *end)
        {
            return self.command(command, backend, info);
        }
        if y < layout.data_start || y >= layout.data_start + layout.rows {
            return Ok(());
        }
        let column = if x >= layout.ascii_start && x < layout.ascii_start + layout.bpr {
            self.pane = Pane::Text;
            x - layout.ascii_start
        } else if x >= layout.hex_start && x < layout.ascii_start - 2 {
            self.pane = Pane::Hex;
            (0..layout.bpr)
                .min_by_key(|index| (layout.hex_start + index * 3 + index / 8).abs_diff(x))
                .unwrap_or(0)
        } else {
            return Ok(());
        };
        let offset = self.top + ((y - layout.data_start) * layout.bpr + column) as u64;
        if event.kind == "down" {
            self.move_to(offset, event.shift, info.size);
            self.dragging = true;
        } else if self.dragging {
            self.anchor.get_or_insert(self.cursor);
            self.cursor = offset.saturating_add(1).min(info.size);
        }
        Ok(())
    }
}

pub fn hex_text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let compact = text
        .replace("0x", "")
        .replace("0X", "")
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '_')
        .collect::<String>();
    if compact.is_empty() {
        return Err("Enter hexadecimal bytes, for example DE AD BE EF".into());
    }
    if compact.len() % 2 != 0 || !compact.bytes().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Enter complete hexadecimal byte pairs, for example DE AD BE EF".into());
    }
    compact
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
                .map_err(|error| error.to_string())
        })
        .collect()
}
