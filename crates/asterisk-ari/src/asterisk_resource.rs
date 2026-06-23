//! /ari/asterisk resource -- system information and management.
//!
//! Port of res/ari/resource_asterisk.c. Provides endpoints for retrieving
//! Asterisk system information (version, uptime), managing modules,
//! reading/writing global variables, and the ping endpoint.

use crate::error::AriErrorKind;
use crate::models::*;
use crate::server::{AriRequest, AriResponse, AriServer, HttpMethod, RestHandler};
use std::sync::Arc;

/// Build the /asterisk route subtree.
pub fn build_asterisk_routes() -> Arc<RestHandler> {
    // /asterisk/ping
    let ping = Arc::new(
        RestHandler::new("ping").on(HttpMethod::Get, handle_ping),
    );

    // /asterisk/info
    let info = Arc::new(
        RestHandler::new("info").on(HttpMethod::Get, handle_get_info),
    );

    // /asterisk/modules/{moduleName}
    let module_by_name = Arc::new(
        RestHandler::new("{moduleName}")
            .on(HttpMethod::Get, handle_get_module)
            .on(HttpMethod::Post, handle_load_module)
            .on(HttpMethod::Put, handle_reload_module)
            .on(HttpMethod::Delete, handle_unload_module),
    );

    // /asterisk/modules
    let modules = Arc::new(
        RestHandler::new("modules")
            .on(HttpMethod::Get, handle_list_modules)
            .child(module_by_name),
    );

    // /asterisk/variable
    let variable = Arc::new(
        RestHandler::new("variable")
            .on(HttpMethod::Get, handle_get_global_variable)
            .on(HttpMethod::Post, handle_set_global_variable),
    );

    // /asterisk/config/dynamic/{configClass}/{objectType}/{id}
    let config_id = Arc::new(
        RestHandler::new("{id}")
            .on(HttpMethod::Get, handle_get_config)
            .on(HttpMethod::Put, handle_update_config)
            .on(HttpMethod::Delete, handle_delete_config),
    );

    let object_type = Arc::new(
        RestHandler::new("{objectType}").child(config_id),
    );

    let config_class = Arc::new(
        RestHandler::new("{configClass}").child(object_type),
    );

    let dynamic = Arc::new(
        RestHandler::new("dynamic").child(config_class),
    );

    let config = Arc::new(
        RestHandler::new("config").child(dynamic),
    );

    // /asterisk
    

    Arc::new(
        RestHandler::new("asterisk")
            .child(ping)
            .child(info)
            .child(modules)
            .child(variable)
            .child(config),
    )
}

// ---------------------------------------------------------------------------
// Handler implementations
// ---------------------------------------------------------------------------

/// GET /asterisk/ping -- ping the server.
fn handle_ping(_req: &AriRequest, _server: &AriServer) -> AriResponse {
    let ping = AsteriskPing {
        asterisk_id: uuid::Uuid::new_v4().to_string(),
        ping: "pong".to_string(),
        timestamp: chrono_now(),
    };
    AriResponse::ok(&ping)
}

/// GET /asterisk/info -- get system information.
fn handle_get_info(req: &AriRequest, _server: &AriServer) -> AriResponse {
    // The ?only= parameter filters which sections to include
    let only: Vec<&str> = req.query_params_multi("only");

    let include_all = only.is_empty();
    let include_build = include_all || only.contains(&"build");
    let include_system = include_all || only.contains(&"system");
    let include_config = include_all || only.contains(&"config");
    let include_status = include_all || only.contains(&"status");

    let info = AsteriskInfo {
        build: if include_build {
            Some(BuildInfo {
                os: std::env::consts::OS.to_string(),
                kernel: String::new(),
                machine: std::env::consts::ARCH.to_string(),
                options: String::new(),
                date: String::new(),
                user: String::new(),
            })
        } else {
            None
        },
        system: if include_system {
            Some(SystemInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
                entity_id: uuid::Uuid::new_v4().to_string(),
            })
        } else {
            None
        },
        config: if include_config {
            Some(ConfigInfo {
                name: "Rustisk".to_string(),
                default_language: "en".to_string(),
                max_channels: None,
                max_open_files: None,
                max_load: None,
                setid: SetId {
                    user: String::new(),
                    group: String::new(),
                },
            })
        } else {
            None
        },
        status: if include_status {
            Some(StatusInfo {
                startup_time: chrono_now(),
                last_reload_time: chrono_now(),
            })
        } else {
            None
        },
    };

    AriResponse::ok(&info)
}

/// GET /asterisk/modules -- list loaded modules.
fn handle_list_modules(_req: &AriRequest, _server: &AriServer) -> AriResponse {
    // In a full implementation, this would query the module registry.
    let modules: Vec<AriModule> = Vec::new();
    AriResponse::ok(&modules)
}

/// GET /asterisk/modules/{moduleName} -- get module details.
fn handle_get_module(req: &AriRequest, _server: &AriServer) -> AriResponse {
    let _module_name = match req.path_var(2) {
        Some(name) => name,
        None => {
            return AriResponse::error(&AriErrorKind::BadRequest(
                "missing moduleName".into(),
            ));
        }
    };

    // In a full implementation, look up the module.
    AriResponse::error(&AriErrorKind::NotFound("Module not found".into()))
}

/// POST /asterisk/modules/{moduleName} -- load a module.
fn handle_load_module(req: &AriRequest, _server: &AriServer) -> AriResponse {
    let _module_name = match req.path_var(2) {
        Some(name) => name,
        None => {
            return AriResponse::error(&AriErrorKind::BadRequest(
                "missing moduleName".into(),
            ));
        }
    };

    // In a full implementation, this would load the module.
    AriResponse::no_content()
}

/// DELETE /asterisk/modules/{moduleName} -- unload a module.
fn handle_unload_module(req: &AriRequest, _server: &AriServer) -> AriResponse {
    let _module_name = match req.path_var(2) {
        Some(name) => name,
        None => {
            return AriResponse::error(&AriErrorKind::BadRequest(
                "missing moduleName".into(),
            ));
        }
    };

    // In a full implementation, this would unload the module.
    AriResponse::no_content()
}

/// PUT /asterisk/modules/{moduleName} -- reload a module.
fn handle_reload_module(req: &AriRequest, _server: &AriServer) -> AriResponse {
    let _module_name = match req.path_var(2) {
        Some(name) => name,
        None => {
            return AriResponse::error(&AriErrorKind::BadRequest(
                "missing moduleName".into(),
            ));
        }
    };

    // In a full implementation, this would reload the module.
    AriResponse::no_content()
}

/// GET /asterisk/variable -- get a global variable.
fn handle_get_global_variable(req: &AriRequest, server: &AriServer) -> AriResponse {
    let variable_name = match req.query_param("variable") {
        Some(v) => v,
        None => {
            return AriResponse::error(&AriErrorKind::BadRequest(
                "missing required parameter: variable".into(),
            ));
        }
    };

    let value = server
        .global_variables
        .get(variable_name)
        .map(|v| v.value().clone())
        .unwrap_or_default();

    AriResponse::ok(&Variable { value })
}

/// POST /asterisk/variable -- set a global variable.
fn handle_set_global_variable(req: &AriRequest, server: &AriServer) -> AriResponse {
    let variable_name = match req.query_param("variable") {
        Some(v) => v.to_string(),
        None => {
            return AriResponse::error(&AriErrorKind::BadRequest(
                "missing required parameter: variable".into(),
            ));
        }
    };

    let value = req.query_param("value").unwrap_or("").to_string();
    server.global_variables.insert(variable_name, value);

    AriResponse::no_content()
}

/// GET /asterisk/config/dynamic/{configClass}/{objectType}/{id} -- get dynamic config.
fn handle_get_config(req: &AriRequest, _server: &AriServer) -> AriResponse {
    let _config_class = req.path_var(3);
    let _object_type = req.path_var(4);
    let _id = req.path_var(5);

    // Sorcery dynamic configuration is not yet implemented
    AriResponse::error(&AriErrorKind::NotFound("Config not found".into()))
}

/// PUT /asterisk/config/dynamic/{configClass}/{objectType}/{id} -- update dynamic config.
fn handle_update_config(req: &AriRequest, _server: &AriServer) -> AriResponse {
    let _config_class = req.path_var(3);
    let _object_type = req.path_var(4);
    let _id = req.path_var(5);

    // Sorcery dynamic configuration is not yet implemented
    AriResponse::error(&AriErrorKind::NotImplemented(
        "dynamic config not yet implemented".into(),
    ))
}

/// DELETE /asterisk/config/dynamic/{configClass}/{objectType}/{id} -- delete dynamic config.
fn handle_delete_config(req: &AriRequest, _server: &AriServer) -> AriResponse {
    let _config_class = req.path_var(3);
    let _object_type = req.path_var(4);
    let _id = req.path_var(5);

    // Sorcery dynamic configuration is not yet implemented
    AriResponse::error(&AriErrorKind::NotImplemented(
        "dynamic config not yet implemented".into(),
    ))
}

/// Get a simple ISO-8601 timestamp string.
fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", now.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{AriConfig, AriServer};

    #[test]
    fn test_ping_response() {
        let server = AriServer::new(AriConfig::default());
        let req = AriRequest {
            method: HttpMethod::Get,
            path: "/ari/asterisk/ping".to_string(),
            path_segments: vec!["ari".into(), "asterisk".into(), "ping".into()],
            query_params: std::collections::HashMap::new(),
            body: None,
            username: None,
        };
        let resp = handle_ping(&req, &server);
        assert_eq!(resp.status, 200);
        let body_str = String::from_utf8_lossy(resp.body.as_ref().unwrap());
        assert!(body_str.contains("pong"));
    }

    #[test]
    fn test_global_variable_set_get() {
        let server = AriServer::new(AriConfig::default());

        // Set a variable
        let mut params = std::collections::HashMap::new();
        params.insert("variable".to_string(), vec!["MYVAR".to_string()]);
        params.insert("value".to_string(), vec!["hello".to_string()]);

        let set_req = AriRequest {
            method: HttpMethod::Post,
            path: "/ari/asterisk/variable".to_string(),
            path_segments: vec!["ari".into(), "asterisk".into(), "variable".into()],
            query_params: params,
            body: None,
            username: None,
        };
        let resp = handle_set_global_variable(&set_req, &server);
        assert_eq!(resp.status, 204);

        // Get the variable
        let mut params = std::collections::HashMap::new();
        params.insert("variable".to_string(), vec!["MYVAR".to_string()]);

        let get_req = AriRequest {
            method: HttpMethod::Get,
            path: "/ari/asterisk/variable".to_string(),
            path_segments: vec!["ari".into(), "asterisk".into(), "variable".into()],
            query_params: params,
            body: None,
            username: None,
        };
        let resp = handle_get_global_variable(&get_req, &server);
        assert_eq!(resp.status, 200);
        let body_str = String::from_utf8_lossy(resp.body.as_ref().unwrap());
        assert!(body_str.contains("hello"));
    }
}
