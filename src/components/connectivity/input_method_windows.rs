use anyhow::{Context, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetKeyboardLayout, GetKeyboardLayoutNameW, HKL, KLF_SETFORPROCESS,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMethodStatus {
    Inactive,
    Active,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputMethodState {
    pub status: InputMethodStatus,
    pub name: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMethodAction {
    Toggle,
}
pub struct Fcitx5Backend;
impl Fcitx5Backend {
    pub fn current(&self) -> Result<InputMethodState> {
        let mut name = [0u16; 9];
        unsafe {
            GetKeyboardLayoutNameW(&mut name).context("get keyboard layout name")?;
        }
        let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        let id = String::from_utf16_lossy(&name[..end]);
        let hkl = unsafe { GetKeyboardLayout(0) };
        let name = layout_label(&id);
        Ok(InputMethodState {
            status: if hkl.0.is_null() {
                InputMethodStatus::Unavailable
            } else if name.contains("pinyin") {
                InputMethodStatus::Active
            } else {
                InputMethodStatus::Inactive
            },
            name: Some(name),
        })
    }
    pub fn apply(&self, _: &InputMethodAction) -> Result<()> {
        // HKL_NEXT is the Win32 sentinel value -1, which windows-rs does not expose.
        let next = HKL((-1_isize) as *mut core::ffi::c_void);
        unsafe { ActivateKeyboardLayout(next, KLF_SETFORPROCESS) }
            .context("activate the next Windows keyboard layout")?;
        Ok(())
    }
}
fn layout_label(id: &str) -> String {
    match id
        .get(id.len().saturating_sub(4)..)
        .unwrap_or(id)
        .to_ascii_lowercase()
        .as_str()
    {
        "0409" => "keyboard-us".into(),
        "0804" | "0404" | "0c04" | "1004" | "1404" => "pinyin-zh".into(),
        _ => id.into(),
    }
}
