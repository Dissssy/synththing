//! Fonts built into synththing for the scripts the UI's own fonts and the
//! pixel font lack: Noto Sans JP and KR Light (`assets/fonts/noto-sans-cjk`,
//! SIL OFL). JP comes first, so the Chinese characters both have are drawn
//! the Japanese way.

pub const CJK: [(&str, &[u8]); 2] = [
    ("NotoSansJP-Light", include_bytes!("../assets/fonts/noto-sans-cjk/NotoSansJP-Light.ttf")),
    ("NotoSansKR-Light", include_bytes!("../assets/fonts/noto-sans-cjk/NotoSansKR-Light.ttf")),
];
