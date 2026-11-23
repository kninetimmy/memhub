use std::borrow::Cow;
use std::fmt;

use rmcp::RoleServer;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, Extensions, GetExtensions, GetMeta, JsonObject,
    JsonRpcMessage, RequestMetaObject,
};
use rmcp::service::{RxJsonRpcMessage, ServiceRole};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// Used only by the SDK's existing byte transport, not by the MCP service.
/// Keep all SDK wire types except for the incoming request's extra observation.
#[derive(Clone, Copy, Debug)]
pub(super) struct StdioWireRole;

impl ServiceRole for StdioWireRole {
    type Req = <RoleServer as ServiceRole>::Req;
    type Resp = <RoleServer as ServiceRole>::Resp;
    type Not = <RoleServer as ServiceRole>::Not;
    type PeerReq = StdioRequest;
    type PeerResp = <RoleServer as ServiceRole>::PeerResp;
    type PeerNot = <RoleServer as ServiceRole>::PeerNot;
    type Info = <RoleServer as ServiceRole>::Info;
    type PeerInfo = <RoleServer as ServiceRole>::PeerInfo;
    type InitializeError = <RoleServer as ServiceRole>::InitializeError;
    const IS_CLIENT: bool = false;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LegacyTaskControl {
    Object,
    Malformed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct StdioRequest {
    // Observe the buffered request map without consuming fields. The SDK
    // still decodes the complete request, including its original extensions.
    #[serde(flatten, deserialize_with = "observe_task", skip_serializing)]
    legacy_task: Option<LegacyTaskControl>,
    #[serde(flatten)]
    request: ClientRequest,
}

impl GetExtensions for StdioRequest {
    fn extensions(&self) -> &Extensions {
        self.request.extensions()
    }

    fn extensions_mut(&mut self) -> &mut Extensions {
        self.request.extensions_mut()
    }
}

impl GetMeta for StdioRequest {
    type Metadata = RequestMetaObject;

    fn get_meta(&self) -> &Self::Metadata {
        self.request.get_meta()
    }

    fn get_meta_mut(&mut self) -> &mut Self::Metadata {
        self.request.get_meta_mut()
    }
}

pub(super) fn into_sdk_message(message: RxJsonRpcMessage<StdioWireRole>) -> ClientJsonRpcMessage {
    match message {
        JsonRpcMessage::Request(message) => {
            let StdioRequest {
                mut request,
                legacy_task,
            } = message.request;
            if let ClientRequest::CallToolRequest(call) = &mut request
                && let Some(task) = legacy_task
            {
                call.extensions.insert(task);
            }
            ClientJsonRpcMessage::request(request, message.id)
        }
        JsonRpcMessage::Notification(message) => ClientJsonRpcMessage::Notification(message),
        JsonRpcMessage::Response(message) => ClientJsonRpcMessage::Response(message),
        JsonRpcMessage::Error(message) => ClientJsonRpcMessage::Error(message),
    }
}

fn observe_task<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<LegacyTaskControl>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Params {
        Ordinary {
            #[serde(rename = "task")]
            _task: Option<()>,
        },
        Augmented {
            #[serde(rename = "task")]
            _task: JsonObject,
        },
        // A wrong type or duplicate task key must never become an ordinary call.
        Malformed(IgnoredAny),
    }

    struct RequestVisitor;
    impl<'de> Visitor<'de> for RequestVisitor {
        type Value = Option<LegacyTaskControl>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an MCP request object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut task = None;
            while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                if key == "params" {
                    let observed = match map.next_value()? {
                        Params::Ordinary { .. } => None,
                        Params::Augmented { .. } => Some(LegacyTaskControl::Object),
                        Params::Malformed(_) => Some(LegacyTaskControl::Malformed),
                    };
                    task = task.or(observed);
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(task)
        }
    }
    deserializer.deserialize_map(RequestVisitor)
}
