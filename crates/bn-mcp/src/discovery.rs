use std::future::Future;

use rmcp::{
    model::{ClientJsonRpcMessage, ClientRequest, ErrorCode, ErrorData, ServerJsonRpcMessage},
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{IntoTransport, Transport},
    RoleServer,
};

/// Let clients probing the 2026-07-28 discovery lifecycle fall back to the initialize lifecycle
/// implemented by rmcp 1.x. Copilot sends this probe before `initialize`; without the adapter,
/// rmcp treats it as an invalid first message and closes the stdio connection.
pub(crate) fn with_legacy_discovery_fallback<T, E, A>(
    transport: T,
) -> impl Transport<RoleServer, Error = E> + 'static
where
    T: IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    LegacyDiscoveryFallback {
        inner: transport.into_transport(),
    }
}

struct LegacyDiscoveryFallback<T> {
    inner: T,
}

impl<T> Transport<RoleServer> for LegacyDiscoveryFallback<T>
where
    T: Transport<RoleServer>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let message = self.inner.receive().await?;
            let request_id = match &message {
                ClientJsonRpcMessage::Request(request)
                    if matches!(
                        &request.request,
                        ClientRequest::CustomRequest(custom)
                            if custom.method == "server/discover"
                    ) =>
                {
                    Some(request.id.clone())
                }
                _ => None,
            };

            let Some(request_id) = request_id else {
                return Some(message);
            };

            let error = ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "Method not found", None);
            if self
                .inner
                .send(ServerJsonRpcMessage::error(error, Some(request_id)))
                .await
                .is_err()
            {
                return None;
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        convert::Infallible,
        sync::{Arc, Mutex},
    };

    use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
    use serde_json::json;

    use super::*;

    struct FakeTransport {
        incoming: VecDeque<ClientJsonRpcMessage>,
        sent: Arc<Mutex<Vec<ServerJsonRpcMessage>>>,
    }

    impl Transport<RoleServer> for FakeTransport {
        type Error = Infallible;

        fn send(
            &mut self,
            item: TxJsonRpcMessage<RoleServer>,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            let sent = Arc::clone(&self.sent);
            async move {
                sent.lock().expect("sent messages lock").push(item);
                Ok(())
            }
        }

        async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
            self.incoming.pop_front()
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn discovery_probe_gets_method_not_found_then_initialize_continues() {
        let discover: ClientJsonRpcMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "server/discover",
            "params": {}
        }))
        .expect("discovery request");
        let initialize: ClientJsonRpcMessage = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}
            }
        }))
        .expect("initialize request");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let inner = FakeTransport {
            incoming: VecDeque::from([discover, initialize.clone()]),
            sent: Arc::clone(&sent),
        };

        let mut transport = with_legacy_discovery_fallback(inner);
        let received = transport.receive().await.expect("initialize message");

        assert_eq!(
            serde_json::to_value(received).expect("serialize received message"),
            serde_json::to_value(initialize).expect("serialize initialize")
        );
        assert_eq!(
            serde_json::to_value(&sent.lock().expect("sent messages lock")[0])
                .expect("serialize discovery response"),
            json!({
                "jsonrpc": "2.0",
                "id": 0,
                "error": {"code": -32601, "message": "Method not found"}
            })
        );
    }
}
