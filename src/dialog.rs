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
/// The packing settings dialog (ConfigurePacker). `info` is shown at the bottom (file
/// locations). Returns the accepted form, already validated by `PackForm::to_options`.
pub fn configure_pack(
    parent: *mut core::ffi::c_void,
    form: &crate::pack_settings::PackForm,
    info: &str,
) -> Option<crate::pack_settings::PackForm> {
    #[cfg(test)]
    {
        let _ = (parent, form, info);
        None
    }
    #[cfg(not(test))]
    imp::configure_pack(parent, form, info)
}

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

    // ---- packing settings (ConfigurePacker)

    use crate::pack_settings::{
        PackForm, COMPRESSIONS, MODES, OODLE_COMPRESSORS, OODLE_LEVELS, ZLIB_LEVELS,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;

    const ID_PMODE: u16 = 300;
    const ID_PCOMP: u16 = 301;
    const ID_PZLIB: u16 = 302;
    const ID_POODLEC: u16 = 303;
    const ID_POODLEL: u16 = 304;
    const ID_PCOMPACT: u16 = 305;
    const ID_PAUTO: u16 = 306;
    const ID_PWRITEPAK: u16 = 307;
    const ID_PWARNPAK: u16 = 308;
    const ID_PMOUNT: u16 = 309;
    const ID_PINFO: u16 = 310;
    const ID_PERROR: u16 = 311;
    const ID_PZLIBLABEL: u16 = 312;
    const ID_POODLELABEL: u16 = 313;
    const CBN_SELCHANGE: u32 = 1;
    const BN_CLICKED: u32 = 0;

    struct PState {
        form: PackForm,
        info: Vec<u16>,
        answer: Option<PackForm>,
    }

    fn pack_template() -> Vec<u32> {
        const BUTTON: u16 = 0x80;
        const EDIT: u16 = 0x81;
        const STATIC: u16 = 0x82;
        const COMBOBOX: u16 = 0x85;
        const SS_NOPREFIX: u32 = 0x80;
        const ES_AUTOHSCROLL: u32 = 0x80;
        const CBS_DROPDOWNLIST: u32 = 3;
        const BS_DEFPUSHBUTTON: u32 = 1;
        const BS_AUTOCHECKBOX: u32 = 3;
        const BS_GROUPBOX: u32 = 7;
        const NONE: u16 = 0xFFFF;
        let list = CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP;
        let mut t = Tpl::dialog("UE IoStore plugin: packing settings", 320, 262);
        // what and how
        t.item(BUTTON, BS_GROUPBOX, 7, 4, 306, 66, NONE, "Packing");
        t.item(STATIC, 0, 14, 18, 64, 9, NONE, "&Mode:");
        t.item(COMBOBOX, list, 80, 16, 226, 80, ID_PMODE, "");
        t.item(STATIC, 0, 14, 35, 64, 9, NONE, "&Compression:");
        t.item(COMBOBOX, list, 80, 33, 90, 120, ID_PCOMP, "");
        t.item(
            STATIC,
            0,
            178,
            35,
            70,
            9,
            ID_PZLIBLABEL,
            "&Zlib/Gzip level:",
        );
        t.item(COMBOBOX, list, 250, 33, 56, 160, ID_PZLIB, "");
        t.item(STATIC, 0, 14, 52, 64, 9, ID_POODLELABEL, "&Oodle:");
        t.item(COMBOBOX, list, 80, 50, 90, 120, ID_POODLEC, "");
        t.item(COMBOBOX, list, 178, 50, 128, 160, ID_POODLEL, "");
        // existing containers
        t.item(
            BUTTON,
            BS_GROUPBOX,
            7,
            74,
            306,
            46,
            NONE,
            "Changing a container",
        );
        t.item(
            BUTTON,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            14,
            87,
            290,
            10,
            ID_PCOMPACT,
            "Re&build after every change (drops the replaced data)",
        );
        t.item(
            STATIC,
            0,
            14,
            103,
            130,
            9,
            NONE,
            "Rebuild containers &smaller than:",
        );
        t.item(
            EDIT,
            ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP,
            148,
            101,
            44,
            13,
            ID_PAUTO,
            "",
        );
        t.item(STATIC, 0, 198, 103, 108, 9, NONE, "(e.g. 10M; 0 = never)");
        // new containers
        t.item(
            BUTTON,
            BS_GROUPBOX,
            7,
            124,
            306,
            76,
            NONE,
            "New container (Alt+F5 to a .ucas that doesn't exist)",
        );
        t.item(
            BUTTON,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            14,
            137,
            290,
            10,
            ID_PWRITEPAK,
            "Also write an empty companion .&pak (the game mounts the container through it)",
        );
        t.item(
            BUTTON,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            26,
            150,
            278,
            10,
            ID_PWARNPAK,
            "&Warn that a .pak is needed when none is written",
        );
        t.item(STATIC, 0, 14, 167, 64, 9, NONE, "Mount &point:");
        t.item(
            EDIT,
            ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP,
            80,
            165,
            226,
            13,
            ID_PMOUNT,
            "",
        );
        t.item(STATIC, SS_NOPREFIX, 80, 181, 226, 16, NONE, "Empty: the common folder of the packed files (as UE does).\nExample: ../../../Game/Content/Mods/");
        // files and errors
        t.item(STATIC, SS_NOPREFIX, 7, 205, 306, 26, ID_PINFO, "");
        t.item(STATIC, SS_NOPREFIX, 7, 232, 196, 22, ID_PERROR, "");
        t.item(
            BUTTON,
            BS_DEFPUSHBUTTON | WS_TABSTOP,
            209,
            241,
            50,
            14,
            1,
            "OK",
        );
        t.item(BUTTON, WS_TABSTOP, 263, 241, 50, 14, 2, "Cancel");
        t.into_dwords()
    }

    unsafe fn fill(hwnd: HWND, id: u16, items: impl Iterator<Item = &'static str>, sel: usize) {
        for it in items {
            let w = wide(it);
            SendDlgItemMessageW(hwnd, id as i32, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        SendDlgItemMessageW(hwnd, id as i32, CB_SETCURSEL, sel, 0);
    }

    unsafe fn sel(hwnd: HWND, id: u16) -> usize {
        let r = SendDlgItemMessageW(hwnd, id as i32, CB_GETCURSEL, 0, 0);
        if r < 0 {
            usize::MAX
        } else {
            r as usize
        }
    }

    unsafe fn checked(hwnd: HWND, id: u16) -> bool {
        SendDlgItemMessageW(hwnd, id as i32, BM_GETCHECK, 0, 0) == 1
    }

    unsafe fn text(hwnd: HWND, id: u16) -> String {
        let mut buf = [0u16; 1024];
        let n = GetDlgItemTextW(hwnd, id as i32, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    unsafe fn read_form(hwnd: HWND) -> PackForm {
        PackForm {
            mode: sel(hwnd, ID_PMODE),
            compression: sel(hwnd, ID_PCOMP),
            zlib_level: sel(hwnd, ID_PZLIB),
            oodle_compressor: sel(hwnd, ID_POODLEC),
            oodle_level: sel(hwnd, ID_POODLEL),
            compact: checked(hwnd, ID_PCOMPACT),
            auto_compact_under: text(hwnd, ID_PAUTO),
            write_empty_pak: checked(hwnd, ID_PWRITEPAK),
            warn_pak_needed: checked(hwnd, ID_PWARNPAK),
            new_mount_point: text(hwnd, ID_PMOUNT),
        }
    }

    /// Greys out what the current choices make irrelevant.
    unsafe fn update_enabled(hwnd: HWND) {
        let f = read_form(hwnd);
        let en = |id: u16, on: bool| {
            EnableWindow(GetDlgItem(hwnd, id as i32), on as i32);
        };
        en(ID_PZLIB, f.zlib_level_used());
        en(ID_PZLIBLABEL, f.zlib_level_used());
        en(ID_POODLEC, f.oodle_used());
        en(ID_POODLEL, f.oodle_used());
        en(ID_POODLELABEL, f.oodle_used());
        en(ID_PWARNPAK, !f.write_empty_pak);
    }

    unsafe extern "system" fn pproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let st = &*(lp as *const PState);
                let f = &st.form;
                fill(hwnd, ID_PMODE, MODES.iter().map(|m| m.0), f.mode);
                fill(
                    hwnd,
                    ID_PCOMP,
                    COMPRESSIONS.iter().map(|m| m.0),
                    f.compression,
                );
                fill(hwnd, ID_PZLIB, ZLIB_LEVELS.iter().copied(), f.zlib_level);
                fill(
                    hwnd,
                    ID_POODLEC,
                    OODLE_COMPRESSORS.iter().map(|m| m.0),
                    f.oodle_compressor,
                );
                fill(
                    hwnd,
                    ID_POODLEL,
                    OODLE_LEVELS.iter().map(|m| m.0),
                    f.oodle_level,
                );
                SendDlgItemMessageW(hwnd, ID_PCOMPACT as i32, BM_SETCHECK, f.compact as usize, 0);
                SendDlgItemMessageW(
                    hwnd,
                    ID_PWRITEPAK as i32,
                    BM_SETCHECK,
                    f.write_empty_pak as usize,
                    0,
                );
                SendDlgItemMessageW(
                    hwnd,
                    ID_PWARNPAK as i32,
                    BM_SETCHECK,
                    f.warn_pak_needed as usize,
                    0,
                );
                SetDlgItemTextW(hwnd, ID_PAUTO as i32, wide(&f.auto_compact_under).as_ptr());
                SetDlgItemTextW(hwnd, ID_PMOUNT as i32, wide(&f.new_mount_point).as_ptr());
                SetDlgItemTextW(hwnd, ID_PINFO as i32, st.info.as_ptr());
                update_enabled(hwnd);
                1
            }
            WM_COMMAND => {
                let id = (wp & 0xFFFF) as u16;
                let code = ((wp >> 16) & 0xFFFF) as u32;
                let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PState;
                if (id == ID_PCOMP && code == CBN_SELCHANGE)
                    || (id == ID_PWRITEPAK && code == BN_CLICKED)
                {
                    update_enabled(hwnd);
                    return 1;
                }
                if id == 1 && !st.is_null() {
                    let f = read_form(hwnd);
                    match f.to_options() {
                        Ok(_) => {
                            (*st).answer = Some(f);
                            EndDialog(hwnd, 1);
                        }
                        Err(e) => {
                            SetDlgItemTextW(
                                hwnd,
                                ID_PERROR as i32,
                                wide(&format!("\u{26A0} {e}")).as_ptr(),
                            );
                        }
                    }
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

    pub fn configure_pack(
        parent: *mut core::ffi::c_void,
        form: &PackForm,
        info: &str,
    ) -> Option<PackForm> {
        let mut st = PState {
            form: form.clone(),
            info: wide(info),
            answer: None,
        };
        let tpl = pack_template();
        let owner = if parent.is_null() {
            unsafe { GetActiveWindow() }
        } else {
            parent as HWND
        };
        let r = unsafe {
            DialogBoxIndirectParamW(
                crate::win::module_handle(),
                tpl.as_ptr() as *const DLGTEMPLATE,
                owner,
                Some(pproc),
                &mut st as *mut PState as LPARAM,
            )
        };
        if r == -1 {
            let err = unsafe { GetLastError() };
            crate::dlog!("packing settings dialog could not be created (error {err})");
            crate::win::error(&format!(
                "The packing settings dialog could not be shown (Windows error {err})."
            ));
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
    pub fn configure_pack(
        _parent: *mut core::ffi::c_void,
        _form: &crate::pack_settings::PackForm,
        _info: &str,
    ) -> Option<crate::pack_settings::PackForm> {
        None
    }
}
