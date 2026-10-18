// pas de console en plus de la fenêtre sur Windows (version release)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    rustscp_lib::run();
}
