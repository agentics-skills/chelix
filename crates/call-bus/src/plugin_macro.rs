//! `plugin!` expands in each plugin crate.

#[macro_export]
macro_rules! plugin {
    (
        name: $name:literal,
        api: $api:literal,
        version: $version:expr,
        description: $description:literal,
        state: $state:ty,
        protocol: $protocol:expr,
        extension: $extension:expr,
        methods: [ $($method_name:literal, $params:ty, $result:ty, $errors:expr, $example:expr, $handler:path),* $(,)? ],
        calls: [ $($call_name:literal, $call_params:ty, $call_result:ty, $call_handler:path),* $(,)? ],
        start: $start:path,
        stop: $stop:path,
    ) => {
        $crate::guest::export_plugin!(__ChelixPlugin with_types_in $crate::guest);

        thread_local! {
            static CHELIX_PLUGIN_STATE: std::cell::RefCell<Option<std::rc::Rc<$state>>> =
                const { std::cell::RefCell::new(None) };
        }

        fn __chelix_plugin_document() -> serde_json::Value {
            let methods = vec![
                $(
                    $crate::guest::method_object(
                        $method_name,
                        $crate::guest::schema_of::<$params>(),
                        $crate::guest::schema_of::<$result>(),
                        &$errors,
                        $example.0,
                        $example.1,
                    ),
                )*
            ];
            $crate::guest::document_value(
                $name,
                $api,
                $version,
                $description,
                $protocol,
                $extension,
                methods,
            )
        }

        struct __ChelixPlugin;

        impl $crate::guest::exports::chelix::plugin::guest::Guest for __ChelixPlugin {
            fn start() -> impl std::future::Future<Output = Result<(), String>> + Send {
                async move {
                    let host = &$crate::guest::Host;
                    let state = $start(host).await?;
                    CHELIX_PLUGIN_STATE.with(|slot| {
                        *slot.borrow_mut() = Some(std::rc::Rc::new(state));
                    });
                    Ok(())
                }
            }

            fn handle(
                request: String,
            ) -> impl std::future::Future<Output = String> + Send {
                async move {
                    let parsed = match $crate::jsonrpc::parse_request(&request) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            return $crate::guest::response_json(None, Err(error));
                        },
                    };
                    if parsed.method == "rpc.discover" {
                        return $crate::guest::response_json(parsed.id, Ok(__chelix_plugin_document()));
                    }
                    let state = CHELIX_PLUGIN_STATE.with(|slot| slot.borrow().clone());
                    let Some(state) = state else {
                        return $crate::guest::response_json(
                            parsed.id,
                            Err($crate::jsonrpc::RpcError::new(
                                $crate::jsonrpc::PROVIDER_UNAVAILABLE,
                                "plugin is not started",
                            )),
                        );
                    };
                    let ctx = $crate::guest::PluginContext::<$state>::from_state(state);
                    let result: Result<serde_json::Value, $crate::jsonrpc::RpcError> = match parsed.method.as_str() {
                        $(
                            concat!($api, ".", $method_name) => {
                                let params: $params = match serde_json::from_value(parsed.params.clone()) {
                                    Ok(params) => params,
                                    Err(error) => {
                                        return $crate::guest::response_json(
                                            parsed.id,
                                            Err($crate::jsonrpc::RpcError::new(
                                                $crate::jsonrpc::INVALID_PARAMS,
                                                error.to_string(),
                                            )),
                                        );
                                    },
                                };
                                match $handler(&ctx, params).await {
                                    Ok(value) => serde_json::to_value(value).map_err(|error| {
                                        $crate::jsonrpc::RpcError::new(
                                            $crate::jsonrpc::INTERNAL_ERROR,
                                            error.to_string(),
                                        )
                                    }),
                                    Err(error) => Err(error),
                                }
                            },
                        )*
                        $( $call_name => {
                            let params: $call_params = match serde_json::from_value(parsed.params.clone()) {
                                Ok(params) => params,
                                Err(error) => {
                                    return $crate::guest::response_json(
                                        parsed.id,
                                        Err($crate::jsonrpc::RpcError::new(
                                            $crate::jsonrpc::INVALID_PARAMS,
                                            error.to_string(),
                                        )),
                                    );
                                },
                            };
                            match $call_handler(&ctx, params).await {
                                Ok(value) => serde_json::to_value(value).map_err(|error| {
                                    $crate::jsonrpc::RpcError::new(
                                        $crate::jsonrpc::INTERNAL_ERROR,
                                        error.to_string(),
                                    )
                                }),
                                Err(error) => Err(error),
                            }
                        }, )*
                        _ => Err($crate::jsonrpc::RpcError::new(
                            $crate::jsonrpc::METHOD_NOT_FOUND,
                            parsed.method.clone(),
                        )),
                    };
                    $crate::guest::response_json(parsed.id, result)
                }
            }

            fn stop() -> impl std::future::Future<Output = ()> + Send {
                async move {
                    let host = &$crate::guest::Host;
                    let state = CHELIX_PLUGIN_STATE.with(|slot| slot.borrow().clone());
                    $stop(host, state.as_deref()).await;
                    CHELIX_PLUGIN_STATE.with(|slot| {
                        *slot.borrow_mut() = None;
                    });
                }
            }
        }
    };
}
