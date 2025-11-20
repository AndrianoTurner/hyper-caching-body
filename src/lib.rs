//! Cacheable HTTP body
//!
//! See [`CachingBody`] for details

//! # Example
//!
//!
//!
//! ```no_run
//!
//! # {
//!use hyper::{server::conn::http1, service::service_fn};
//!use hyper_caching_body::CachingBody;
//!use hyper_util::rt::TokioIo;
//!use std::net::SocketAddr;
//!use tokio::net::{TcpListener, TcpStream};
//!
//!#[tokio::main]
//!async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!    let in_addr: SocketAddr = ([127, 0, 0, 1], 3001).into();
//!    let out_addr: SocketAddr = ([127, 0, 0, 1], 8080).into();
//!
//!    let out_addr_clone = out_addr;
//!
//!    let listener = TcpListener::bind(in_addr).await?;
//!
//!    println!("Listening on http://{}", in_addr);
//!    println!("Proxying on http://{}", out_addr);
//!
//!    loop {
//!        let (stream, _) = listener.accept().await?;
//!        let io = TokioIo::new(stream);
//!
//!        let service = service_fn(move |mut req| {
//!            let uri_string = format!(
//!                "http://{}{}",
//!                out_addr_clone,
//!                req.uri()
//!                    .path_and_query()
//!                    .map(|x| x.as_str())
//!                    .unwrap_or("/")
//!            );
//!            let uri = uri_string.parse().unwrap();
//!            *req.uri_mut() = uri;
//!
//!            let host = req.uri().host().expect("uri has no host");
//!            let port = req.uri().port_u16().unwrap_or(80);
//!            let addr = format!("{}:{}", host, port);
//!
//!            async move {
//!                let client_stream = TcpStream::connect(addr).await.unwrap();
//!                let io = TokioIo::new(client_stream);
//!
//!                let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
//!                tokio::task::spawn(async move {
//!                    if let Err(err) = conn.await {
//!                        println!("Connection failed: {:?}", err);
//!                    }
//!                });
//!
//!                // Here we create a channel to receive buffer contents in another task
//!
//!                let (tx, mut rx) = tokio::sync::mpsc::channel(1);
//!                let res = sender
//!                    .send_request(req)
//!                    .await
//!                    .map(|r| r.map(|b| CachingBody::new(b, tx))); // Wrap the body
//!
//!                // Spawn a task to receive buffe`r contents in another task
//!                tokio::task::spawn(async move {
//!                    if let Some(body) = rx.recv().await {
//!                        dbg!(body);
//!                    }
//!                });
//!                res
//!            }
//!        });
//!
//!        tokio::task::spawn(async move {
//!            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
//!                println!("Failed to serve the connection: {:?}", err);
//!            }
//!        });
//!    }
//!}
//! # }
//! ```
use bytes::{Bytes, BytesMut};
use std::sync::Mutex;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

/// A wrapper for [`hyper::body::Incoming`] that caches the contents in-flight
/// On each
#[derive(Debug)]
pub struct CachingBody {
    /// Inner body
    body: hyper::body::Incoming,
    /// Zero-copy buffer for received content
    buf: Arc<Mutex<BytesMut>>,
    /// Sender for buffered body
    tx: tokio::sync::mpsc::Sender<Bytes>,
}

impl CachingBody {
    /// Create a [`CachingBody`] wrapping [`hyper::body::Incoming`]
    ///
    /// On each successful poll with [`Some(Ok(http_body::Frame<bytes::Bytes>))`] writes to the underlying buf
    ///
    /// When the underlying [`hyper::body::Incoming`] reaches the end of stream it is dropped and the buf is sent over a channel
    pub fn new(body: hyper::body::Incoming, tx: tokio::sync::mpsc::Sender<Bytes>) -> Self {
        let buf = Arc::new(Mutex::new(BytesMut::new()));
        Self { body, buf, tx }
    }
}

impl Drop for CachingBody {
    // This is a hack?
    //
    // So for some reason poll_frame on Incoming body never returns a Poll::Ready(None) e.g. the end is reached (maybe i'm dumb)
    fn drop(&mut self) {
        let guard_res = self.buf.lock();
        if let Ok(guard) = guard_res {
            let bytes = guard.clone().freeze();
            let _ = self.tx.try_send(bytes);
        }
    }
}

impl hyper::body::Body for CachingBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<core::result::Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
        let incoming = &mut self.body;
        let poll = Pin::new(incoming).poll_frame(cx);

        match poll {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && let Ok(mut guard) = self.buf.lock()
                {
                    guard.extend_from_slice(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }

            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),

            Poll::Ready(None) => Poll::Ready(None),

            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}
