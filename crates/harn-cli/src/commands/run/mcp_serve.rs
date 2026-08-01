pub(crate) enum RunFileMcpServeMode {
    Stdio,
    Http(Box<RunFileMcpServeHttp>),
    App(Box<RunFileAppServe>),
}

pub(crate) struct RunFileMcpServeHttp {
    pub options: harn_serve::McpHttpServeOptions,
    pub auth_policy: harn_serve::AuthPolicy,
}

pub(crate) struct RunFileAppServe {
    pub bind: std::net::SocketAddr,
    pub resource: Option<String>,
    pub open: bool,
}

pub(super) async fn run_server(
    server: harn_vm::McpServer,
    mut vm: harn_vm::Vm,
    mode: RunFileMcpServeMode,
) {
    let result = match mode {
        RunFileMcpServeMode::Stdio => server.run(&mut vm).await.map_err(|error| error.to_string()),
        RunFileMcpServeMode::Http(http) => {
            let RunFileMcpServeHttp {
                options,
                auth_policy,
            } = *http;
            crate::commands::serve::run_script_mcp_http_server(server, vm, options, auth_policy)
                .await
        }
        RunFileMcpServeMode::App(app) => {
            let RunFileAppServe {
                bind,
                resource,
                open,
            } = *app;
            crate::commands::app::run_script_app_server(server, vm, bind, resource, open).await
        }
    };
    if let Err(error) = result {
        eprintln!("error: MCP server error: {error}");
        std::process::exit(1);
    }
}
