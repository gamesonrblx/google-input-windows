//! COM class and language-profile ids for the 粵拼 text service.
//! Fixed so re-registering updates the same Windows input method.

use windows_sys::core::GUID;

pub const JYUTPING: &str = "yue-hant-t-i0-jyutping";
pub const DESCRIPTION: &str = "Chinese (Jyutping)";
/// Traditional Chinese. The zh-Hant-HK language list addresses input methods as 0404.
pub const LANGID: u16 = 0x0404;
/// US QWERTY, so Jyutping letters come from the physical keyboard.
pub const HKL_US: isize = 0x04090409;

pub const CLSID: GUID = guid(0xC4A7E2B1, 0x91D4, 0x4F0A, [0xB8, 0x3E, 0x7A, 0x16, 0x5D, 0x90, 0x2C, 0x48]);
pub const PROFILE: GUID = guid(0xD19B6F30, 0x4E27, 0x4C85, [0x9F, 0x01, 0xA6, 0x3C, 0x88, 0xE1, 0x5B, 0x72]);

pub const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> GUID {
    GUID { data1, data2, data3, data4 }
}

pub fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

pub fn guid_braced(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
    )
}

/// Value name Windows stores under `zh-Hant-HK` in the user language profile.
pub fn tip_id() -> String {
    format!("0404:{}{}", guid_braced(&CLSID), guid_braced(&PROFILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_id_matches_the_language_list_shape() {
        let id = tip_id();
        assert!(id.starts_with("0404:{C4A7E2B1-91D4-4F0A-B83E-7A165D902C48}{"));
        assert!(id.ends_with("{D19B6F30-4E27-4C85-9F01-A63C88E15B72}"));
        assert_eq!(id.matches('{').count(), 2);
    }
}
