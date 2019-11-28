use futures::FutureExt;
use std::{
    future::Future,
    io,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    task::{Context, Poll},
    vec::IntoIter,
};

use crate::{
    multiaddr::{multihash::Multihash, Multiaddr, Protocol},
    secio::PeerId,
    utils::{extract_peer_id, is_ws, socketaddr_to_multiaddr},
};

/// DNS resolver, use on multi-thread tokio runtime
pub struct DNSResolver {
    source_address: Multiaddr,
    peer_id: Option<PeerId>,
    ws: bool,
    port: u16,
    domain: String,
    join_handle: Option<tokio::task::JoinHandle<::std::io::Result<IntoIter<SocketAddr>>>>,
}

impl DNSResolver {
    /// If address like `/dns4/localhost/tcp/80` or `"/dns6/localhost/tcp/80"`,
    /// it will be return Some, else None
    pub fn new(source_address: Multiaddr) -> Option<Self> {
        let mut iter = source_address.iter().peekable();

        let (domain, port) = loop {
            if iter.peek().is_none() {
                break (None, None);
            }
            match iter.peek() {
                Some(Protocol::Dns4(_)) | Some(Protocol::Dns6(_)) => (),
                _ => {
                    let _ = iter.next();
                    continue;
                }
            }

            let proto1 = iter.next()?;
            let proto2 = iter.next()?;

            match (proto1, proto2) {
                (Protocol::Dns4(domain), Protocol::Tcp(port)) => break (Some(domain), Some(port)),
                (Protocol::Dns6(domain), Protocol::Tcp(port)) => break (Some(domain), Some(port)),
                _ => (),
            }
        };

        match (domain, port) {
            (Some(domain), Some(port)) => Some(DNSResolver {
                peer_id: extract_peer_id(&source_address),
                ws: is_ws(&source_address),
                domain: domain.to_string(),
                source_address,
                port,
                join_handle: None,
            }),
            _ => None,
        }
    }

    fn new_addr(
        &mut self,
        mut iter: IntoIter<SocketAddr>,
    ) -> Poll<Result<Multiaddr, (Multiaddr, io::Error)>> {
        match iter.next() {
            Some(address) => {
                let mut address = socketaddr_to_multiaddr(address);

                if self.ws {
                    address.push(Protocol::Ws);
                }
                if let Some(peer_id) = self.peer_id.take() {
                    address.push(Protocol::P2p(
                        Multihash::from_bytes(peer_id.into_bytes()).expect("Invalid peer id"),
                    ))
                }
                Poll::Ready(Ok(address))
            }
            None => Poll::Ready(Err((
                self.source_address.clone(),
                io::ErrorKind::InvalidData.into(),
            ))),
        }
    }
}

impl Future for DNSResolver {
    type Output = Result<Multiaddr, (Multiaddr, io::Error)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.join_handle.is_none() {
            let domain = self.domain.clone();
            let port = self.port;

            self.join_handle = Some(tokio::task::spawn_blocking(move || {
                (&domain[..], port).to_socket_addrs()
            }));
        }

        let mut handle = self.join_handle.take().unwrap();

        match handle.poll_unpin(cx) {
            Poll::Pending => {
                self.join_handle = Some(handle);
                Poll::Pending
            }
            Poll::Ready(res) => match res {
                Ok(Ok(iter)) => self.new_addr(iter),
                Err(e) => Poll::Ready(Err((self.source_address.clone(), e.into()))),
                Ok(Err(e)) => Poll::Ready(Err((self.source_address.clone(), e))),
            },
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        multiaddr::{Multiaddr, Protocol},
        utils::dns::DNSResolver,
    };

    #[test]
    fn dns_parser() {
        let future: DNSResolver =
            DNSResolver::new("/dns4/localhost/tcp/80".parse().unwrap()).unwrap();
        let mut rt = tokio::runtime::Runtime::new().unwrap();
        let addr = rt.block_on(future).unwrap();
        match addr.iter().next().unwrap() {
            Protocol::Ip4(_) => {
                assert_eq!("/ip4/127.0.0.1/tcp/80".parse::<Multiaddr>().unwrap(), addr)
            }
            Protocol::Ip6(_) => assert_eq!("/ip6/::1/tcp/80".parse::<Multiaddr>().unwrap(), addr),
            _ => panic!("Dns resolver fail"),
        }
    }
}
