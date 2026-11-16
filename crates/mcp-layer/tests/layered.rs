use mcp_layer::{Layered, Routed};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ServerCapabilities, ServerConfig},
    tool, tool_router,
};

#[derive(Clone)]
struct First;

#[tool_router]
impl First {
    #[tool(description = "first tool")]
    async fn zebra(&self) -> String {
        "first".into()
    }
}

#[derive(Clone)]
struct Second;

#[tool_router]
impl Second {
    #[tool(description = "second tool")]
    async fn alpha(&self) -> String {
        "second".into()
    }
}

fn server() -> Layered {
    let info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
    Layered::new(info)
        .with(Routed { state: First, router: First::tool_router() })
        .with(Routed { state: Second, router: Second::tool_router() })
}

#[tokio::test]
async fn routes_over_duplex() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        server().serve(server_io).await.unwrap().waiting().await.unwrap();
    });
    let client = ().serve(client_io).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(), ["alpha", "zebra"]);
    for (name, expected) in [("zebra", "first"), ("alpha", "second")] {
        let result = client.call_tool(CallToolRequestParams::new(name)).await.unwrap();
        assert_eq!(result.content[0].as_text().unwrap().text, expected);
    }
    assert!(client.call_tool(CallToolRequestParams::new("missing")).await.is_err());
    client.cancel().await.unwrap();
    server.await.unwrap();
}

#[test]
#[should_panic(expected = "duplicate tool name")]
fn duplicate_names_panic() {
    server().with(Routed { state: First, router: First::tool_router() });
}
