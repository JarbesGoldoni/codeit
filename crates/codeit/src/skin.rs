//! A look a plugin binary can give the web app, in VS Code's own theme formats, so a theme you
//! use there works here: its colors for the app and the code, its file and folder icons, its
//! icons for the app's buttons. codeit ships none; without one the app keeps its own look.

/// VS Code theme files, served to the app under `/skin/`.
pub struct Skin {
    /// Shown in the app.
    pub name: String,
    /// A color theme (`colors`, `tokenColors`).
    pub theme: Option<&'static str>,
    /// A file icon theme (`iconDefinitions`, `fileExtensions`, `folderNames`…).
    pub icons: Option<&'static str>,
    /// A product icon theme (`fonts`, `iconDefinitions` of font characters).
    pub product_icons: Option<&'static str>,
    /// The files the themes point to (icons, fonts), by their path in the theme's JSON, which
    /// must be relative to `/skin/` (`icons/files/rust.svg`).
    pub file: fn(&str) -> Option<&'static [u8]>,
}
