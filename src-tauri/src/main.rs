// CodeWave 桌面应用入口（薄壳）：只调用 lib::run()
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    codewave_lib::run()
}
