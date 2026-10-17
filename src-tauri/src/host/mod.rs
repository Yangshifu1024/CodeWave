// 宿主层。仅 `commands` / `events` / `notify` 允许 `use tauri::*`
// （[docs/technical-design](../../docs/technical-design.md)）。
//
pub mod commands;
pub mod events;
pub mod keyring;
pub mod notify;
pub mod trust;
