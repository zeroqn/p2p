use bytes::Bytes;
use futures::prelude::*;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    task::Context,
    time::Duration,
};

use crate::{
    buffer::{PriorityBuffer, SendResult},
    channel::{mpsc, mpsc::Priority},
    error::SendErrorKind,
    multiaddr::Multiaddr,
    protocol_select::ProtocolInfo,
    secio::{PublicKey, SecioKeyPair},
    service::{event::ServiceTask, ServiceControl, SessionType, TargetProtocol, TargetSession},
    session::SessionEvent,
    ProtocolId, SessionId,
};

pub(crate) struct SessionController {
    pub(crate) buffer: PriorityBuffer<SessionEvent>,
    pub(crate) inner: Arc<SessionContext>,
}

impl SessionController {
    pub(crate) fn new(
        event_sender: mpsc::Sender<SessionEvent>,
        inner: Arc<SessionContext>,
    ) -> Self {
        Self {
            buffer: PriorityBuffer::new(event_sender),
            inner,
        }
    }

    pub(crate) fn push(&mut self, priority: Priority, event: SessionEvent) {
        if priority.is_high() {
            self.buffer.push_high(event)
        } else {
            self.buffer.push_normal(event)
        }
    }

    pub(crate) fn push_message(&mut self, proto_id: ProtocolId, priority: Priority, data: Bytes) {
        self.inner.incr_pending_data_size(data.len());
        let message_event = SessionEvent::ProtocolMessage {
            id: self.inner.id,
            proto_id,
            data,
        };
        self.push(priority, message_event)
    }

    pub(crate) fn try_send(&mut self, cx: &mut Context) -> SendResult {
        self.buffer.try_send(cx)
    }
}

/// Session context, contains basic information about the current connection
#[derive(Clone, Debug)]
pub struct SessionContext {
    /// Session's ID
    pub id: SessionId,
    /// Remote socket address
    pub address: Multiaddr,
    /// Session type (server or client)
    pub ty: SessionType,
    // TODO: use reference?
    /// Remote public key
    pub remote_pubkey: Option<PublicKey>,
    pub(crate) closed: Arc<AtomicBool>,
    pending_data_size: Arc<AtomicUsize>,
}

impl SessionContext {
    pub(crate) fn new(
        id: SessionId,
        address: Multiaddr,
        ty: SessionType,
        remote_pubkey: Option<PublicKey>,
        closed: Arc<AtomicBool>,
        pending_data_size: Arc<AtomicUsize>,
    ) -> SessionContext {
        SessionContext {
            id,
            address,
            ty,
            remote_pubkey,
            closed,
            pending_data_size,
        }
    }

    // Increase when data pushed to Service's write buffer
    pub(crate) fn incr_pending_data_size(&self, data_size: usize) {
        #[cfg(feature = "metrics")]
        crate::metrics::TENTACLE_MESSAGE_IN_TX_QUEUE.inc();

        self.pending_data_size
            .fetch_add(data_size, Ordering::Relaxed);
    }

    // Decrease when data sent to underlying Yamux Stream
    pub(crate) fn decr_pending_data_size(&self, data_size: usize) {
        #[cfg(feature = "metrics")]
        crate::metrics::TENTACLE_MESSAGE_IN_TX_QUEUE.dec();

        self.pending_data_size
            .fetch_sub(data_size, Ordering::Relaxed);
    }

    /// Session is closed
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    /// Session pending data size
    pub fn pending_data_size(&self) -> usize {
        self.pending_data_size.load(Ordering::Relaxed)
    }
}

type Result = std::result::Result<(), SendErrorKind>;

/// The Service runtime can send some instructions to the inside of the handle.
/// This is the sending channel.
// TODO: Need to maintain the network topology map here?
pub struct ServiceContext {
    listens: Vec<Multiaddr>,
    key_pair: Option<SecioKeyPair>,
    inner: ServiceControl,
}

impl ServiceContext {
    /// New
    pub(crate) fn new(
        task_sender: mpsc::UnboundedSender<ServiceTask>,
        proto_infos: HashMap<ProtocolId, ProtocolInfo>,
        key_pair: Option<SecioKeyPair>,
        closed: Arc<AtomicBool>,
    ) -> Self {
        ServiceContext {
            inner: ServiceControl::new(task_sender, proto_infos, closed),
            key_pair,
            listens: Vec::new(),
        }
    }

    /// Create a new listener
    #[inline]
    pub fn listen(&self, address: Multiaddr) -> Result {
        self.inner.listen(address)
    }

    /// Initiate a connection request to address
    #[inline]
    pub fn dial(&self, address: Multiaddr, target: TargetProtocol) -> Result {
        self.inner.dial(address, target)
    }

    /// Disconnect a connection
    #[inline]
    pub fn disconnect(&self, session_id: SessionId) -> Result {
        self.inner.disconnect(session_id)
    }

    /// Send message
    #[inline]
    pub fn send_message_to(
        &self,
        session_id: SessionId,
        proto_id: ProtocolId,
        data: Bytes,
    ) -> Result {
        self.inner.send_message_to(session_id, proto_id, data)
    }

    /// Send message on quick channel
    #[inline]
    pub fn quick_send_message_to(
        &self,
        session_id: SessionId,
        proto_id: ProtocolId,
        data: Bytes,
    ) -> Result {
        self.inner.quick_send_message_to(session_id, proto_id, data)
    }

    /// Send data to the specified protocol for the specified sessions.
    #[inline]
    pub fn filter_broadcast(
        &self,
        session_ids: TargetSession,
        proto_id: ProtocolId,
        data: Bytes,
    ) -> Result {
        self.inner.filter_broadcast(session_ids, proto_id, data)
    }

    /// Send data to the specified protocol for the specified sessions on quick channel.
    #[inline]
    pub fn quick_filter_broadcast(
        &self,
        session_ids: TargetSession,
        proto_id: ProtocolId,
        data: Bytes,
    ) -> Result {
        self.inner
            .quick_filter_broadcast(session_ids, proto_id, data)
    }

    /// Send a future task
    #[inline]
    pub fn future_task<T>(&self, task: T) -> Result
    where
        T: Future<Output = ()> + 'static + Send,
    {
        self.inner.future_task(task)
    }

    /// Try open a protocol
    ///
    /// If the protocol has been open, do nothing
    #[inline]
    pub fn open_protocol(&self, session_id: SessionId, proto_id: ProtocolId) -> Result {
        self.inner.open_protocol(session_id, proto_id)
    }

    /// Try open protocol
    ///
    /// If the protocol has been open, do nothing
    #[inline]
    pub fn open_protocols(&self, session_id: SessionId, target: TargetProtocol) -> Result {
        self.inner.open_protocols(session_id, target)
    }

    /// Try close a protocol
    ///
    /// If the protocol has been closed, do nothing
    #[inline]
    pub fn close_protocol(&self, session_id: SessionId, proto_id: ProtocolId) -> Result {
        self.inner.close_protocol(session_id, proto_id)
    }

    /// Get the internal channel sender side handle
    #[inline]
    pub fn control(&self) -> &ServiceControl {
        &self.inner
    }

    /// Get service protocol message, Map(ID, Name), but can't modify
    #[inline]
    pub fn protocols(&self) -> &Arc<HashMap<ProtocolId, ProtocolInfo>> {
        &self.inner.proto_infos
    }

    /// Get the key pair of self
    #[inline]
    pub fn key_pair(&self) -> Option<&SecioKeyPair> {
        self.key_pair.as_ref()
    }

    /// Get service listen address list
    #[inline]
    pub fn listens(&self) -> &[Multiaddr] {
        self.listens.as_ref()
    }

    /// Update listen list
    #[inline]
    pub(crate) fn update_listens(&mut self, address_list: Vec<Multiaddr>) {
        self.listens = address_list;
    }

    /// Set a service notify token
    pub fn set_service_notify(
        &self,
        proto_id: ProtocolId,
        interval: Duration,
        token: u64,
    ) -> Result {
        self.inner.set_service_notify(proto_id, interval, token)
    }

    /// Set a session notify token
    pub fn set_session_notify(
        &self,
        session_id: SessionId,
        proto_id: ProtocolId,
        interval: Duration,
        token: u64,
    ) -> Result {
        self.inner
            .set_session_notify(session_id, proto_id, interval, token)
    }

    /// Remove a service timer by a token
    pub fn remove_service_notify(&self, proto_id: ProtocolId, token: u64) -> Result {
        self.inner.remove_service_notify(proto_id, token)
    }

    /// Remove a session timer by a token
    pub fn remove_session_notify(
        &self,
        session_id: SessionId,
        proto_id: ProtocolId,
        token: u64,
    ) -> Result {
        self.inner
            .remove_session_notify(session_id, proto_id, token)
    }

    /// Close service.
    ///
    /// Order:
    /// 1. close all listens
    /// 2. try close all session's protocol stream
    /// 3. try close all session
    /// 4. close service
    pub fn close(&self) -> Result {
        self.inner.close()
    }

    /// Shutdown service, don't care anything, may cause partial message loss
    pub fn shutdown(&self) -> Result {
        self.inner.shutdown()
    }

    pub(crate) fn clone_self(&self) -> Self {
        ServiceContext {
            inner: self.inner.clone(),
            key_pair: self.key_pair.clone(),
            listens: self.listens.clone(),
        }
    }
}

/// Protocol handle context
pub struct ProtocolContext {
    inner: ServiceContext,
    /// Protocol id
    pub proto_id: ProtocolId,
}

impl ProtocolContext {
    pub(crate) fn new(service_context: ServiceContext, proto_id: ProtocolId) -> Self {
        ProtocolContext {
            inner: service_context,
            proto_id,
        }
    }

    #[inline]
    pub(crate) fn as_mut<'a, 'b: 'a>(
        &'b mut self,
        session: &'a SessionContext,
    ) -> ProtocolContextMutRef<'a> {
        ProtocolContextMutRef {
            inner: self,
            session,
        }
    }
}

/// Protocol handle context contain session context
pub struct ProtocolContextMutRef<'a> {
    inner: &'a mut ProtocolContext,
    /// Session context
    pub session: &'a SessionContext,
}

impl<'a> ProtocolContextMutRef<'a> {
    /// Send message to current protocol current session
    #[inline]
    pub fn send_message(&self, data: Bytes) -> Result {
        let proto_id = self.proto_id();
        self.inner.send_message_to(self.session.id, proto_id, data)
    }

    /// Send message to current protocol current session on quick channel
    #[inline]
    pub fn quick_send_message(&self, data: Bytes) -> Result {
        let proto_id = self.proto_id();
        self.inner
            .quick_send_message_to(self.session.id, proto_id, data)
    }

    /// Protocol id
    #[inline]
    pub fn proto_id(&self) -> ProtocolId {
        self.inner.proto_id
    }
}

impl Deref for ProtocolContext {
    type Target = ServiceContext;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for ProtocolContext {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<'a> Deref for ProtocolContextMutRef<'a> {
    type Target = ProtocolContext;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> DerefMut for ProtocolContextMutRef<'a> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
