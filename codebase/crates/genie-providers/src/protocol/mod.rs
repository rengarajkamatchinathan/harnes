//! The `Protocol` trait (03 §3) and the four v1 adapters (03 §5).

pub mod anthropic;
pub mod gemini;
pub mod openai_chat;
pub mod openai_responses;
mod util;

use std::sync::Arc;

use serde::Serialize;

use crate::compat::Compat;
use crate::compile::CompiledRequest;
use crate::error::{HttpFailure, ProviderError};
use crate::event::StreamEvent;
use crate::model::{Origin, ProtocolKind, ReplayScope, WireRecord};

pub use util::WireBody;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    Sse,
    /// SSE terminated by `data: [DONE]`.
    SseWithDone,
    AwsEventStream,
    JsonLines,
}

/// One framed unit off the wire (an SSE event).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawFrame {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

impl RawFrame {
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
            id: None,
        }
    }
}

/// Per-stream context handed to the decoder before the first frame.
#[derive(Clone, Debug)]
pub struct StreamContext {
    /// Origin stamped on opaque payloads produced by this stream.
    pub origin: Origin,
    pub compat: Compat,
}

/// A built request body plus the wire records for messages normalised in this request.
pub struct BuiltBody<B> {
    pub body: B,
    pub records: Vec<WireRecord>,
}

pub trait Protocol: Send + Sync + 'static {
    type Body: Serialize + Send;
    type Frame: Send;
    type State: Default + Send;
    const KIND: ProtocolKind;
    const FRAMING: Framing;
    const REPLAY: ReplayScope;

    fn build_body(
        &self,
        req: &CompiledRequest,
        compat: &Compat,
    ) -> Result<BuiltBody<Self::Body>, ProviderError>;
    /// `None` = unknown or ignorable frame (raw passthrough only).
    fn decode(&self, frame: &RawFrame) -> Result<Option<Self::Frame>, ProviderError>;
    /// Called once before the first frame.
    fn begin(&self, _st: &mut Self::State, _ctx: &StreamContext) {}
    fn step(
        &self,
        st: &mut Self::State,
        f: Self::Frame,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError>;
    /// End of body. No terminal event → `IncompleteStream`.
    fn on_halt(&self, st: Self::State, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>;
    fn classify(&self, failure: &HttpFailure) -> ProviderError;
}

/// Decoder for one response stream.
pub trait StreamDecoder: Send {
    fn feed(&mut self, frame: &RawFrame, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>;
    fn finish(self: Box<Self>, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>;
}

/// Object-safe view of a [`Protocol`], used by [`crate::Route`].
pub trait ErasedProtocol: Send + Sync {
    fn kind(&self) -> ProtocolKind;
    fn framing(&self) -> Framing;
    fn replay(&self) -> ReplayScope;
    fn build(
        &self,
        req: &CompiledRequest,
        compat: &Compat,
    ) -> Result<BuiltBody<Vec<u8>>, ProviderError>;
    fn decoder(&self, ctx: &StreamContext) -> Box<dyn StreamDecoder>;
    fn classify(&self, failure: &HttpFailure) -> ProviderError;
}

struct Decoder<P: Protocol> {
    p: Arc<P>,
    st: P::State,
}

impl<P: Protocol> StreamDecoder for Decoder<P> {
    fn feed(&mut self, frame: &RawFrame, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        match self.p.decode(frame)? {
            Some(f) => self.p.step(&mut self.st, f, out),
            None => Ok(()),
        }
    }
    fn finish(self: Box<Self>, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let me = *self;
        me.p.on_halt(me.st, out)
    }
}

/// Wraps a [`Protocol`] as an [`ErasedProtocol`].
pub struct Erased<P>(pub Arc<P>);

impl<P: Protocol> ErasedProtocol for Erased<P> {
    fn kind(&self) -> ProtocolKind {
        P::KIND
    }
    fn framing(&self) -> Framing {
        P::FRAMING
    }
    fn replay(&self) -> ReplayScope {
        P::REPLAY
    }
    fn build(
        &self,
        req: &CompiledRequest,
        compat: &Compat,
    ) -> Result<BuiltBody<Vec<u8>>, ProviderError> {
        let b = self.0.build_body(req, compat)?;
        let ser =
            |e: serde_json::Error| ProviderError::invalid_request(format!("serialise body: {e}"));
        let patches: Vec<_> = req
            .request
            .model
            .patches
            .iter()
            .filter_map(|p| p.body.as_ref())
            .collect();
        let bytes = if patches.is_empty() {
            serde_json::to_vec(&b.body).map_err(ser)?
        } else {
            // Body patches (catalog quirks) are top-level knobs. Going through `Value`
            // re-orders object keys, so routes that need byte-exact history replay must not
            // carry body patches; the catalog patch tests pin this.
            let mut v = serde_json::to_value(&b.body).map_err(ser)?;
            for p in patches {
                crate::compat::merge_patch(&mut v, p);
            }
            serde_json::to_vec(&v).map_err(ser)?
        };
        Ok(BuiltBody {
            body: bytes,
            records: b.records,
        })
    }
    fn decoder(&self, ctx: &StreamContext) -> Box<dyn StreamDecoder> {
        let mut st = P::State::default();
        self.0.begin(&mut st, ctx);
        Box::new(Decoder {
            p: self.0.clone(),
            st,
        })
    }
    fn classify(&self, failure: &HttpFailure) -> ProviderError {
        self.0.classify(failure)
    }
}

/// The adapter for a protocol kind (Bedrock is v2 → `None`).
pub fn for_kind(kind: ProtocolKind) -> Option<Arc<dyn ErasedProtocol>> {
    Some(match kind {
        ProtocolKind::AnthropicMessages => Arc::new(Erased(Arc::new(anthropic::AnthropicMessages))),
        ProtocolKind::OpenaiResponses => {
            Arc::new(Erased(Arc::new(openai_responses::OpenAiResponses)))
        }
        ProtocolKind::OpenaiChat => Arc::new(Erased(Arc::new(openai_chat::OpenAiChat))),
        ProtocolKind::Gemini => Arc::new(Erased(Arc::new(gemini::Gemini))),
        ProtocolKind::BedrockConverse => return None,
    })
}

/// Decode a whole recorded SSE body through a protocol (tests and cassette tooling).
pub fn decode_all(
    p: &dyn ErasedProtocol,
    ctx: &StreamContext,
    body: &[u8],
) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let mut parser = crate::sse::SseParser::new();
    let mut frames = vec![];
    parser.push(body, &mut frames);
    parser.finish(&mut frames);
    let mut dec = p.decoder(ctx);
    let mut out = vec![];
    for f in &frames {
        if let Err(e) = dec.feed(f, &mut out) {
            return (out, Some(e));
        }
    }
    let err = dec.finish(&mut out).err();
    (out, err)
}
