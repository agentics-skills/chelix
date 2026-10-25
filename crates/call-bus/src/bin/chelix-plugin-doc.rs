//! Load a plugin component and print its `rpc.discover` document.

use std::env;
use std::process::ExitCode;

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

struct HostState {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: chelix-plugin-doc <plugin.wasm>");
        return ExitCode::from(2);
    };
    match print_document(&path) {
        Ok(document) => {
            println!("{document}");
            ExitCode::SUCCESS
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        },
    }
}

fn print_document(path: &str) -> Result<String, String> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    config.concurrency_support(true);
    let engine = Engine::new(&config).map_err(|error| error.to_string())?;
    let component = Component::from_file(&engine, path).map_err(|error| error.to_string())?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|error| error.to_string())?;
    let mut store = Store::new(
        &engine,
        HostState {
            ctx: WasiCtx::builder().build(),
            table: ResourceTable::new(),
        },
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let instance = linker
            .instantiate_async(&mut store, &component)
            .await
            .map_err(|error| error.to_string())?;
        let guest = instance
            .get_export_index(&mut store, None, "chelix:plugin/guest@1.0.0")
            .or_else(|| instance.get_export_index(&mut store, None, "chelix:plugin/guest"))
            .ok_or_else(|| "guest export is missing".to_owned())?;
        let handle = instance
            .get_export_index(&mut store, Some(&guest), "handle")
            .ok_or_else(|| "handle export is missing".to_owned())?;
        let func = instance
            .get_typed_func::<(String,), (String,)>(&mut store, &handle)
            .map_err(|error| error.to_string())?;
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"rpc.discover","params":{}}"#.to_owned();
        let (response,) = func
            .call_async(&mut store, (request,))
            .await
            .map_err(|error| error.to_string())?;
        let value: serde_json::Value =
            serde_json::from_str(&response).map_err(|error| error.to_string())?;
        value
            .get("result")
            .map(|result| result.to_string())
            .ok_or_else(|| response)
    })
}
