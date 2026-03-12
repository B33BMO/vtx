use std::path::Path;
use std::sync::{Arc, Mutex};

use tracing::{error, info, warn};
use wasmtime::*;

use crate::hooks::{HookContext, HookEvent};
use crate::lua_plugin::PluginAction;

/// A single loaded WASM plugin.
pub struct WasmPlugin {
    pub name: String,
    store: Store<PluginState>,
    instance: Instance,
    /// WASM plugins respond to all hooks via a single `on_hook` export.
    /// We track which hooks exist based on the exports available.
    pub has_on_hook: bool,
    pub has_on_load: bool,
}

/// Host state accessible from WASM host functions.
struct PluginState {
    /// Memory exported by the WASM module.
    memory: Option<Memory>,
    /// Pending actions from this plugin.
    actions: Arc<Mutex<Vec<PluginAction>>>,
}

impl WasmPlugin {
    /// Load a WASM plugin from a `.wasm` file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let wasm_bytes = std::fs::read(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

        Self::load_from_bytes(&name, &wasm_bytes)
    }

    /// Load a WASM plugin from raw bytes.
    pub fn load_from_bytes(name: &str, wasm_bytes: &[u8]) -> Result<Self, String> {
        let engine = Engine::default();
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|e| format!("failed to compile WASM module '{name}': {e}"))?;

        let actions: Arc<Mutex<Vec<PluginAction>>> = Arc::new(Mutex::new(Vec::new()));

        let mut store = Store::new(
            &engine,
            PluginState {
                memory: None,
                actions: Arc::clone(&actions),
            },
        );

        let mut linker = Linker::new(&engine);

        // Host function: vtx_log(ptr, len)
        linker
            .func_wrap("env", "vtx_log", |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                let msg = read_wasm_string(&mut caller, ptr, len);
                info!(target: "vtx_plugin_wasm", "{msg}");
            })
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_send_keys(pane_id, ptr, len)
        linker
            .func_wrap(
                "env",
                "vtx_send_keys",
                |mut caller: Caller<'_, PluginState>, pane_id: i32, ptr: i32, len: i32| {
                    let data = read_wasm_bytes(&mut caller, ptr, len);
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::SendKeys {
                            pane_id: pane_id as u32,
                            data,
                        });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_split(horizontal)
        linker
            .func_wrap(
                "env",
                "vtx_split",
                |caller: Caller<'_, PluginState>, horizontal: i32| {
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::Split {
                            horizontal: horizontal != 0,
                        });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_notify(ptr, len)
        linker
            .func_wrap(
                "env",
                "vtx_notify",
                |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                    let message = read_wasm_string(&mut caller, ptr, len);
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::Notify { message });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_new_window(ptr, len) — name string (empty = no name)
        linker
            .func_wrap(
                "env",
                "vtx_new_window",
                |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                    let name = if len > 0 {
                        Some(read_wasm_string(&mut caller, ptr, len))
                    } else {
                        None
                    };
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::NewWindow { name });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_run_command(ptr, len) — command string
        linker
            .func_wrap(
                "env",
                "vtx_run_command",
                |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                    let command = read_wasm_string(&mut caller, ptr, len);
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::RunCommand { command });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_set_layout(ptr, len) — preset name string
        linker
            .func_wrap(
                "env",
                "vtx_set_layout",
                |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                    let preset = read_wasm_string(&mut caller, ptr, len);
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::SetLayout { preset });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_rename_window(ptr, len) — name string
        linker
            .func_wrap(
                "env",
                "vtx_rename_window",
                |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                    let name = read_wasm_string(&mut caller, ptr, len);
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::RenameWindow { name });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_zoom()
        linker
            .func_wrap(
                "env",
                "vtx_zoom",
                |caller: Caller<'_, PluginState>| {
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::ZoomPane);
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_select_window(index)
        linker
            .func_wrap(
                "env",
                "vtx_select_window",
                |caller: Caller<'_, PluginState>, index: i32| {
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::SelectWindow {
                            index: index as usize,
                        });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_kill_pane()
        linker
            .func_wrap(
                "env",
                "vtx_kill_pane",
                |caller: Caller<'_, PluginState>| {
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::KillPane);
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        // Host function: vtx_popup(ptr, len) — command string (empty = no command)
        linker
            .func_wrap(
                "env",
                "vtx_popup",
                |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                    let command = if len > 0 {
                        Some(read_wasm_string(&mut caller, ptr, len))
                    } else {
                        None
                    };
                    caller
                        .data()
                        .actions
                        .lock()
                        .unwrap()
                        .push(PluginAction::Popup { command });
                },
            )
            .map_err(|e| format!("linker error: {e}"))?;

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("failed to instantiate WASM module '{name}': {e}"))?;

        // Grab the exported memory.
        if let Some(memory) = instance.get_memory(&mut store, "memory") {
            store.data_mut().memory = Some(memory);
        }

        let has_on_load = instance.get_typed_func::<(), ()>(&mut store, "on_load").is_ok();
        let has_on_hook = instance
            .get_typed_func::<(i32, i32, i32, i32), ()>(&mut store, "on_hook")
            .is_ok();

        let mut plugin = Self {
            name: name.to_string(),
            store,
            instance,
            has_on_hook,
            has_on_load,
        };

        // Call on_load if exported.
        if plugin.has_on_load {
            plugin.call_on_load();
        }

        Ok(plugin)
    }

    /// Call the plugin's `on_load` export.
    fn call_on_load(&mut self) {
        let func = match self.instance.get_typed_func::<(), ()>(&mut self.store, "on_load") {
            Ok(f) => f,
            Err(_) => return,
        };
        if let Err(e) = func.call(&mut self.store, ()) {
            error!(plugin = %self.name, "on_load error: {e}");
        }
    }

    /// Dispatch a hook event to this WASM plugin.
    pub fn dispatch_hook(&mut self, event: HookEvent, ctx: &HookContext) -> Vec<PluginAction> {
        if !self.has_on_hook {
            return Vec::new();
        }

        self.store.data().actions.lock().unwrap().clear();

        let hook_name = event.as_str();
        let ctx_json = serde_json::to_string(ctx).unwrap_or_default();

        // Write hook_name and ctx_json into WASM memory via an allocator,
        // or if the module doesn't export an allocator, we skip.
        let alloc = match self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "alloc")
        {
            Ok(f) => f,
            Err(_) => {
                warn!(plugin = %self.name, "WASM plugin has on_hook but no alloc export; skipping");
                return Vec::new();
            }
        };

        let result: Result<(), String> = (|| {
            let hook_name_ptr = self.write_to_wasm(&alloc, hook_name.as_bytes())?;
            let ctx_ptr = self.write_to_wasm(&alloc, ctx_json.as_bytes())?;

            let on_hook = self
                .instance
                .get_typed_func::<(i32, i32, i32, i32), ()>(&mut self.store, "on_hook")
                .map_err(|e| format!("get on_hook: {e}"))?;

            on_hook
                .call(
                    &mut self.store,
                    (
                        hook_name_ptr,
                        hook_name.len() as i32,
                        ctx_ptr,
                        ctx_json.len() as i32,
                    ),
                )
                .map_err(|e| format!("on_hook call: {e}"))?;

            Ok(())
        })();

        if let Err(e) = result {
            error!(plugin = %self.name, "dispatch_hook error: {e}");
        }

        self.drain_actions()
    }

    /// Write bytes into WASM memory using the plugin's alloc function.
    fn write_to_wasm(
        &mut self,
        alloc: &TypedFunc<i32, i32>,
        data: &[u8],
    ) -> Result<i32, String> {
        let ptr = alloc
            .call(&mut self.store, data.len() as i32)
            .map_err(|e| format!("alloc({}) failed: {e}", data.len()))?;

        let memory = self
            .store
            .data()
            .memory
            .ok_or("no exported memory")?;

        let mem_data = memory.data_mut(&mut self.store);
        let start = ptr as usize;
        let end = start + data.len();
        if end > mem_data.len() {
            return Err("write out of bounds".into());
        }
        mem_data[start..end].copy_from_slice(data);
        Ok(ptr)
    }

    /// Drain pending actions.
    fn drain_actions(&self) -> Vec<PluginAction> {
        self.store
            .data()
            .actions
            .lock()
            .unwrap()
            .drain(..)
            .collect()
    }
}

/// Read a UTF-8 string from WASM memory.
fn read_wasm_string(caller: &mut Caller<'_, PluginState>, ptr: i32, len: i32) -> String {
    let bytes = read_wasm_bytes(caller, ptr, len);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Read raw bytes from WASM memory.
fn read_wasm_bytes(caller: &mut Caller<'_, PluginState>, ptr: i32, len: i32) -> Vec<u8> {
    let memory = match caller.data().memory {
        Some(m) => m,
        None => return Vec::new(),
    };
    let data = memory.data(caller);
    let start = ptr as usize;
    let end = start + len as usize;
    if end > data.len() {
        return Vec::new();
    }
    data[start..end].to_vec()
}
