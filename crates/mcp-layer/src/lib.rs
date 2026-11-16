//! Composable MCP tool sets.

pub mod job_tools;
pub mod jobs;
pub mod profiles;

use std::{collections::HashSet, future::Future, pin::Pin};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::tool::{ToolCallContext, ToolRouter},
    model::{
        CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
        ServerConfig, Tool,
    },
    service::RequestContext,
};

pub type CallFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CallToolResponse, ErrorData>> + Send + 'a>>;

pub trait ToolSet: Send + Sync + 'static {
    fn tools(&self) -> Vec<Tool>;
    fn has(&self, name: &str) -> bool;
    fn call(&self, req: CallToolRequestParams, cx: RequestContext<RoleServer>) -> CallFuture<'_>;
}

/// A state and its rmcp tool router.
pub struct Routed<S> {
    pub state: S,
    pub router: ToolRouter<S>,
}

impl<S: Send + Sync + 'static> ToolSet for Routed<S> {
    fn tools(&self) -> Vec<Tool> {
        self.router.list_all()
    }

    fn has(&self, name: &str) -> bool {
        self.router.has_route(name)
    }

    fn call(&self, req: CallToolRequestParams, cx: RequestContext<RoleServer>) -> CallFuture<'_> {
        Box::pin(self.router.call(ToolCallContext::new(&self.state, req, cx)))
    }
}

/// Composes independent routers; rmcp 3.10 uses `ListToolsResult::with_all_items`.
pub struct Layered {
    info: ServerConfig,
    sets: Vec<Box<dyn ToolSet>>,
}

impl Layered {
    pub fn new(info: ServerConfig) -> Self {
        Self {
            info,
            sets: Vec::new(),
        }
    }

    /// Add a tool set. Panics if any listed name is duplicated, including within the set.
    pub fn with(mut self, set: impl ToolSet) -> Self {
        let mut names: HashSet<_> = self
            .sets
            .iter()
            .flat_map(|s| s.tools())
            .map(|tool| tool.name)
            .collect();
        for tool in set.tools() {
            assert!(
                names.insert(tool.name.clone()),
                "duplicate tool name: {}",
                tool.name
            );
        }
        self.sets.push(Box::new(set));
        self
    }

    pub async fn serve_stdio(self) -> anyhow::Result<()> {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
        self.serve(rmcp::transport::stdio())
            .await?
            .waiting()
            .await?;
        Ok(())
    }
}

impl ServerHandler for Layered {
    fn get_info(&self) -> ServerConfig {
        self.info.clone()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools: Vec<_> = self.sets.iter().flat_map(|set| set.tools()).collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ListToolsResult::with_all_items(tools))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.sets
            .iter()
            .flat_map(|set| set.tools())
            .find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        req: CallToolRequestParams,
        cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match self.sets.iter().find(|set| set.has(&req.name)) {
            Some(set) => set.call(req, cx).await,
            None => Err(ErrorData::invalid_params("tool not found", None)),
        }
    }
}
