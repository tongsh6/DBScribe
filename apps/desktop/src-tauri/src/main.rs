// Windows 下发布时隐藏控制台窗口，仅作 release 构建
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dbs_desktop_lib::run()
}
