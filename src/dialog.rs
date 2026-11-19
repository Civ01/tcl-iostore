//! Modal AES key prompt, built from an in-memory dialog template
//! (no resource compiler needed). Same dialog as tcl-uepak.

use std::path::Path;

/// What the user entered.
pub struct KeyAnswer {
    pub text: String,
    pub remember: bool,
}

pub struct KeyPrompt<'a> {
    pub container: &'a Path,
    pub guid: &'a str,
    /// Shown above the buttons, e.g. "Key does not match this container".
    pub error: Option<&'a str>,
}

#[cfg(test)]
pub static TEST_ANSWERS: std::sync::Mutex<Vec<Option<(String, bool)>>> =
    std::sync::Mutex::new(Vec::new());

/// Number of key prompts shown (tests).
#[cfg(test)]
pub static TEST_PROMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Error line of the last key prompt shown (tests).
#[cfg(test)]
pub static TEST_LAST_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub static TEST_CONFIRMS: std::sync::Mutex<Vec<bool>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
pub static TEST_CONFIRM_TEXTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Yes/No question. Tests answer from `TEST_CONFIRMS` (empty = No).
pub fn confirm(text: &str) -> bool {
    #[cfg(test)]
    {
        TEST_CONFIRM_TEXTS.lock().unwrap().push(text.to_string());
        let mut q = TEST_CONFIRMS.lock().unwrap();
        if q.is_empty() {
            false
        } else {
            q.remove(0)
        }
    }
    #[cfg(not(test))]
    crate::win::ask_yes_no(text)
}

/// Engine-version prompt for the legacy view (SPEC §6a).
pub struct VersionPrompt<'a> {
    pub container: &'a Path,
    pub choices: &'a [&'a str],
    pub selected: usize,
    /// The choices are the versions that fit the container (false: none fit, all listed).
    pub known: bool,
}

pub struct VersionAnswer {
    pub index: usize,
    /// Save to uetoc.ini.
    pub remember: bool,
    /// Don't ask again until another container is opened.
    pub session: bool,
}

/// (index, remember, session) answers for tests; empty = Cancel.
#[cfg(test)]
pub static TEST_VERSION_ANSWERS: std::sync::Mutex<Vec<(usize, bool, bool)>> = std::sync::Mutex::new(Vec::new());
/// (choices, selected) of each version prompt shown (tests).
#[cfg(test)]
pub static TEST_VERSION_PROMPTS: std::sync::Mutex<Vec<(Vec<String>, usize)>> = std::sync::Mutex::new(Vec::new());

pub fn prompt_version(p: &VersionPrompt) -> Option<VersionAnswer> {
    #[cfg(test)]
    {
        TEST_VERSION_PROMPTS.lock().unwrap().push((p.choices.iter().map(|s| s.to_string()).collect(), p.selected));
        let mut q = TEST_VERSION_ANSWERS.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        let (index, remember, session) = q.remove(0);
        Some(VersionAnswer { index, remember, session })
    }
    #[cfg(not(test))]
    imp::prompt_version(p)
}

/// Shows the dialog. `None` = cancelled.
pub fn prompt_key(p: &KeyPrompt) -> Option<KeyAnswer> {
    #[cfg(test)]
    {
        TEST_PROMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *TEST_LAST_ERROR.lock().unwrap() = p.error.map(String::from);
        let mut q = TEST_ANSWERS.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        q.remove(0)
            .map(|(text, remember)| KeyAnswer { text, remember })
    }
    #[cfg(not(test))]
    imp::prompt(p)
}

#[cfg(all(windows, not(test)))]
mod imp {
    use super::*;
    use crate::win::wide;
    use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const ID_INFO: u16 = 100;
    const ID_EDIT: u16 = 101;
    const ID_SESSION: u16 = 102;
    const ID_ERROR: u16 = 103;
    const ID_LABEL: u16 = 104;
    const BM_GETCHECK: u32 = 0x00F0;
    const EM_LIMITTEXT: u32 = 0x00C5;

    struct State {
        info: Vec<u16>,
        error: Vec<u16>,
        answer: Option<KeyAnswer>,
    }

    /// DLGTEMPLATE builder (all fields are WORD/DWORD sequences). Counts the
    /// items itself: a wrong count in the header makes Windows read past the
    /// template.
    pub(super) struct Tpl {
        buf: Vec<u16>,
        count_at: usize,
        items: u16,
    }
    impl Tpl {
        /// Dialog header: modal frame, caption, centered, MS Shell Dlg 2 8pt.
        pub(super) fn dialog(title: &str, cx: i16, cy: i16) -> Self {
            const DS_SETFONT: u32 = 0x40;
            const DS_MODALFRAME: u32 = 0x80;
            const DS_FIXEDSYS: u32 = 0x8;
            const DS_CENTER: u32 = 0x800;
            // background operations run in threads without a window: come
            // to the front anyway
            const DS_SETFOREGROUND: u32 = 0x200;
            let mut t = Tpl {
                buf: vec![],
                count_at: 0,
                items: 0,
            };
            t.u32(
                DS_SETFONT
                    | DS_FIXEDSYS
                    | DS_MODALFRAME
                    | DS_CENTER
                    | DS_SETFOREGROUND
                    | WS_POPUP
                    | WS_CAPTION
                    | WS_SYSMENU,
            );
            t.u32(0);
            t.count_at = t.buf.len();
            t.u16(0); // item count, patched in into_dwords
            for v in [0i16, 0, cx, cy] {
                t.u16(v as u16);
            }
            t.u16(0); // menu
            t.u16(0); // class
            t.str(title);
            t.u16(8);
            t.str("MS Shell Dlg 2");
            t
        }
        fn u32(&mut self, v: u32) {
            self.buf.push(v as u16);
            self.buf.push((v >> 16) as u16);
        }
        fn u16(&mut self, v: u16) {
            self.buf.push(v);
        }
        fn str(&mut self, s: &str) {
            self.buf.extend(s.encode_utf16());
            self.buf.push(0);
        }
        fn align4(&mut self) {
            if self.buf.len() % 2 == 1 {
                self.buf.push(0);
            }
        }
        #[allow(clippy::too_many_arguments)]
        pub(super) fn item(
            &mut self,
            class: u16,
            style: u32,
            x: i16,
            y: i16,
            cx: i16,
            cy: i16,
            id: u16,
            text: &str,
        ) {
            self.align4();
            self.u32(style | WS_CHILD | WS_VISIBLE);
            self.u32(0);
            for v in [x, y, cx, cy] {
                self.u16(v as u16);
            }
            self.u16(id);
            self.u16(0xFFFF);
            self.u16(class);
            self.str(text);
            self.u16(0);
            self.items += 1;
        }
        /// DWORD-aligned template for DialogBoxIndirect.
        pub(super) fn into_dwords(mut self) -> Vec<u32> {
            self.buf[self.count_at] = self.items;
            if self.buf.len() % 2 == 1 {
                self.buf.push(0);
            }
            self.buf
                .chunks(2)
                .map(|w| w[0] as u32 | (w[1] as u32) << 16)
                .collect()
        }
    }

    fn template() -> Vec<u32> {
        const BUTTON: u16 = 0x80;
        const EDIT: u16 = 0x81;
        const STATIC: u16 = 0x82;
        const SS_NOPREFIX: u32 = 0x80;
        const ES_AUTOHSCROLL: u32 = 0x80;
        const BS_DEFPUSHBUTTON: u32 = 1;
        const BS_AUTOCHECKBOX: u32 = 3;
        let mut t = Tpl::dialog("UE IoStore plugin: AES key needed", 310, 132);
        t.item(STATIC, SS_NOPREFIX, 7, 7, 296, 34, ID_INFO, "");
        t.item(
            STATIC,
            0,
            7,
            44,
            296,
            9,
            ID_LABEL,
            "AES key (64 hex digits, optional 0x, or base64):",
        );
        t.item(
            EDIT,
            ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP,
            7,
            55,
            296,
            14,
            ID_EDIT,
            "",
        );
        t.item(
            BUTTON,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            7,
            74,
            296,
            10,
            ID_SESSION,
            "Use for this &session only (don't save it to uetoc.keys.json)",
        );
        t.item(STATIC, SS_NOPREFIX, 7, 88, 296, 18, ID_ERROR, "");
        t.item(
            BUTTON,
            BS_DEFPUSHBUTTON | WS_TABSTOP,
            199,
            111,
            50,
            14,
            1,
            "OK",
        );
        t.item(BUTTON, WS_TABSTOP, 253, 111, 50, 14, 2, "Cancel");
        t.into_dwords()
    }

    unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let st = &*(lp as *const State);
                SetDlgItemTextW(hwnd, ID_INFO as i32, st.info.as_ptr());
                SetDlgItemTextW(hwnd, ID_ERROR as i32, st.error.as_ptr());
                SendDlgItemMessageW(hwnd, ID_EDIT as i32, EM_LIMITTEXT, 200, 0);
                1 // focus goes to the first tab stop: the edit box
            }
            WM_COMMAND => {
                let id = (wp & 0xFFFF) as i32;
                let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if id == 1 && !st.is_null() {
                    let mut buf = [0u16; 256];
                    let n =
                        GetDlgItemTextW(hwnd, ID_EDIT as i32, buf.as_mut_ptr(), buf.len() as i32);
                    // unticked (default): saved for this folder and its subfolders
                    let remember =
                        SendDlgItemMessageW(hwnd, ID_SESSION as i32, BM_GETCHECK, 0, 0) != 1;
                    (*st).answer = Some(KeyAnswer {
                        text: String::from_utf16_lossy(&buf[..n as usize]),
                        remember,
                    });
                    EndDialog(hwnd, 1);
                    return 1;
                }
                if id == 2 {
                    EndDialog(hwnd, 0);
                    return 1;
                }
                0
            }
            _ => 0,
        }
    }

    pub fn prompt(p: &KeyPrompt) -> Option<KeyAnswer> {
        let name = p
            .container
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folder = p
            .container
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let guid = if p.guid == crate::keys::ZERO_GUID {
            format!("{} (main key)", p.guid)
        } else {
            p.guid.to_string()
        };
        let mut st = State {
            info: wide(&format!(
                "This IoStore container is encrypted.\nContainer: {name}\nFolder: {folder}\nKey GUID: {guid}"
            )),
            error: wide(&p.error.map(|e| format!("\u{26A0} {e}")).unwrap_or_default()),
            answer: None,
        };
        let tpl = template();
        let r = unsafe {
            DialogBoxIndirectParamW(
                crate::win::module_handle(),
                tpl.as_ptr() as *const DLGTEMPLATE,
                GetActiveWindow(),
                Some(proc_),
                &mut st as *mut State as LPARAM,
            )
        };
        if r == -1 {
            let err = unsafe { GetLastError() };
            crate::dlog!("key dialog could not be created (error {err})");
            crate::win::error(&format!(
                "The AES key dialog could not be shown (Windows error {err})."
            ));
        }
        st.answer
    }


    // ---- engine version prompt (legacy view)

    const ID_VINFO: u16 = 200;
    const ID_VLIST: u16 = 201;
    const ID_VREMEMBER: u16 = 202;
    const ID_VSESSION: u16 = 203;
    const CB_ADDSTRING: u32 = 0x0143;
    const CB_SETCURSEL: u32 = 0x014E;
    const CB_GETCURSEL: u32 = 0x0147;
    const BM_SETCHECK: u32 = 0x00F1;

    struct VState {
        info: Vec<u16>,
        choices: Vec<Vec<u16>>,
        selected: usize,
        answer: Option<VersionAnswer>,
    }

    fn version_template() -> Vec<u32> {
        const BUTTON: u16 = 0x80;
        const STATIC: u16 = 0x82;
        const COMBOBOX: u16 = 0x85;
        const SS_NOPREFIX: u32 = 0x80;
        const CBS_DROPDOWNLIST: u32 = 3;
        const BS_DEFPUSHBUTTON: u32 = 1;
        const BS_AUTOCHECKBOX: u32 = 3;
        let mut t = Tpl::dialog("UE IoStore plugin: engine version", 310, 150);
        t.item(STATIC, SS_NOPREFIX, 7, 7, 296, 58, ID_VINFO, "");
        t.item(STATIC, 0, 7, 70, 80, 9, 0xFFFF, "&Engine version:");
        t.item(COMBOBOX, CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP, 90, 68, 120, 120, ID_VLIST, "");
        t.item(BUTTON, BS_AUTOCHECKBOX | WS_TABSTOP, 7, 88, 296, 10, ID_VREMEMBER, "&Remember (save as LegacyEngineVersion in uetoc.ini)");
        t.item(BUTTON, BS_AUTOCHECKBOX | WS_TABSTOP, 7, 102, 296, 10, ID_VSESSION, "&Don't ask again for this container (until another one is opened)");
        t.item(BUTTON, BS_DEFPUSHBUTTON | WS_TABSTOP, 199, 129, 50, 14, 1, "OK");
        t.item(BUTTON, WS_TABSTOP, 253, 129, 50, 14, 2, "Cancel");
        t.into_dwords()
    }

    unsafe extern "system" fn vproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let st = &*(lp as *const VState);
                SetDlgItemTextW(hwnd, ID_VINFO as i32, st.info.as_ptr());
                for c in &st.choices {
                    SendDlgItemMessageW(hwnd, ID_VLIST as i32, CB_ADDSTRING, 0, c.as_ptr() as LPARAM);
                }
                SendDlgItemMessageW(hwnd, ID_VLIST as i32, CB_SETCURSEL, st.selected, 0);
                SendDlgItemMessageW(hwnd, ID_VREMEMBER as i32, BM_SETCHECK, 1, 0);
                1
            }
            WM_COMMAND => {
                let id = (wp & 0xFFFF) as i32;
                let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut VState;
                if id == 1 && !st.is_null() {
                    let sel = SendDlgItemMessageW(hwnd, ID_VLIST as i32, CB_GETCURSEL, 0, 0);
                    (*st).answer = Some(VersionAnswer {
                        index: if sel < 0 { (*st).selected } else { sel as usize },
                        remember: SendDlgItemMessageW(hwnd, ID_VREMEMBER as i32, BM_GETCHECK, 0, 0) == 1,
                        session: SendDlgItemMessageW(hwnd, ID_VSESSION as i32, BM_GETCHECK, 0, 0) == 1,
                    });
                    EndDialog(hwnd, 1);
                    return 1;
                }
                if id == 2 {
                    EndDialog(hwnd, 0);
                    return 1;
                }
                0
            }
            _ => 0,
        }
    }

    pub fn prompt_version(p: &VersionPrompt) -> Option<VersionAnswer> {
        let name = p.container.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let why = if p.known {
            "The packages carry no engine version, and the container's format is written by more than one engine version."
        } else {
            "The packages carry no engine version, and the container's format doesn't tell which engine wrote it."
        };
        let info = format!(
            "Converting to legacy files (.uasset/.uexp) needs the engine version.\n{why}\nPick the game's engine version.\nContainer: {name}"
        );
        let mut st = VState { info: wide(&info), choices: p.choices.iter().map(|c| wide(c)).collect(), selected: p.selected, answer: None };
        let tpl = version_template();
        let r = unsafe {
            DialogBoxIndirectParamW(crate::win::module_handle(), tpl.as_ptr() as *const DLGTEMPLATE, GetActiveWindow(), Some(vproc), &mut st as *mut VState as LPARAM)
        };
        if r == -1 {
            let err = unsafe { GetLastError() };
            crate::dlog!("version dialog could not be created (error {err})");
        }
        st.answer
    }
}


#[cfg(all(not(windows), not(test)))]
mod imp {
    use super::*;
    pub fn prompt(_p: &KeyPrompt) -> Option<KeyAnswer> {
        None
    }
    pub fn prompt_version(_p: &VersionPrompt) -> Option<VersionAnswer> {
        None
    }
}
