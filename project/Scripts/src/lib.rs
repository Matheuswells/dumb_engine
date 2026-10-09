//! Game scripts. Every `.rs` file in this folder is a script module and is found
//! automatically (see build.rs); modules with `pub fn register(reg: &mut Registry)`
//! are registered with the engine. Create scripts from the editor (Scripts ▸ New Script).

include!(concat!(env!("OUT_DIR"), "/scripts.rs"));

dumb_script::export_plugin!(register_scripts);
