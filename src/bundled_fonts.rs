//! Fonts built into synththing on top of egui's own (SIL OFL, under
//! `assets/fonts`): Noto Sans JP and KR Light for the scripts the UI's own
//! fonts and the pixel font lack (JP first, so the Chinese characters both
//! have are drawn the Japanese way), and Noto Emoji Light, the one source of
//! emoji, so they're drawn flat and the same on every platform.

pub const CJK: [(&str, &[u8]); 2] = [
    ("NotoSansJP-Light", include_bytes!("../assets/fonts/noto-sans-cjk/NotoSansJP-Light.ttf")),
    ("NotoSansKR-Light", include_bytes!("../assets/fonts/noto-sans-cjk/NotoSansKR-Light.ttf")),
];

pub const EMOJI: (&str, &[u8]) = ("NotoEmoji-Light", include_bytes!("../assets/fonts/noto-emoji/NotoEmoji-Light.ttf"));
