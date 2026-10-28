//! File-based custom editor API.
use serde::{Deserialize, Serialize};
pub mod action {
    pub const UNDO: u32 = 1;
    pub const RELOAD: u32 = 9;
}
#[link(wasm_import_module = "vtcode")]
#[cfg(target_arch = "wasm32")]
unsafe extern "C" {
    fn host_set_view_input_pending(id: *const u8, id_len: i32, pending: i32);
    fn host_document_read(id: u32, offset: i64, ptr: *mut u8, max: i32) -> i32;
}
pub(crate) async fn compute<'a, T: Trait + ?Sized>(mut value: &'a T) -> Result<u32, Error>
where T: Send + Sync {
    let answer = if true { Some(42u32) } else { None };
    for index in 0..=10 { if index == 3 { continue; } }
    while let Some(item) = answer { break; }
    match answer { Some(number) => return Ok(number), _ => panic!("missing") }
    async move { compute(value).await };
    loop { unsafe { core::ptr::read(value) }; break; }
}
pub struct Record { pub field: bool, name: String }
enum Choice { First, Second(u8), Third { field: char } }
trait Trait: Sized { fn run(&self) -> Self; }
impl Trait for Record { fn run(&self) -> Self { todo!() } }
type Alias = Vec<Option<Record>>;
static mut GLOBAL: usize = 0;
union Bits { integer: u32, float: f32 }
extern crate std as standard;
fn parameters(name: *const u8, name_len: i32, text: &str, byte: u8) {
    let r#const = "const mod fn let struct";
    let raw = r###"raw \" contents # mod const"###;
    let bytes = b"\x41\n";
    let character = '\u{1f980}';
    let number = 0xff_u32 + 0b1010 + 0o77 + 1.25e-2_f64;
    let flag = true && !false || number >= 1.0;
    let item = &mut GLOBAL;
    let slice = &item[0..3];
    let name = crate::action::UNDO;
    let result = super::module::function::<Record>();
    let value = self.method()?.field;
    let closure = |value: u32| value + 1;
    'outer: for item in slice { break 'outer; }
    macro_rules! example { ($value:expr) => { println!("{value:?}") }; }
    SomeType!(r#type);
}
// mod const fn are comments here.
/* outer mod /* nested const */ still a comment */
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::module) struct Decorated;
let text = r#"first line
mod const fn are a raw string here
last line"#;
let unicode = "λ 🦀";
let café = 1;
abstract become box do final gen macro override priv ref try typeof unsized virtual yield
