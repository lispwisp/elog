/*
    TODO:
        single threaded TCP/UDP server
        [✓] exit condition for client and server
        [] assert test conditions
        [] consult reference for client and server and make robust
        [] extract into their own functions, pass arguments in from thread scope
        [] convert this unit test into a bench, use criterion?
        [] arbitrarily large input and output buffers (read_vectored, write_vectored)
        [] in place data reduction inside pages in i/o buffers (base64 decode in place)
        [] in place data expansion inside pages in i/o buffers (decompression in place)
        [] server connection pooling and recycling
        [] live telemetry
        [] buffering via mmap pages (for gigantic buffers)
        [] use thread locals for the actual buffer and other variables

        sts load balancer
        [] load balance component (against multiple single threaded servers)

        HTTP parsing as a processing step
        [] http 1.0/2.0 parser function
        [] tls function

        HTTP handling as a processing step
        [] redirect function
        [] other http headers, etc functions

        TCP/UDP proxy
        [] function to establish a connection and send request along

        Message Queue
        [] just link together a bunch of proxies which have gigantic mmap buffers; that is basically a queue
        [] client can read from any machine in the chain for pull messages
        [] machines in chain connect (and re-establish if lost) to parent
        [] arbitrary topologies are possible; redundancy possible
            an efficient topology would likely use a queue of addresses at which to find actual elements in a cache or long term storage.

        Cache
        [] the proxy merely writes a copy of file to disk after retrieving it; may be some clever pagination tricks that can be done here.

        Key value store
        [] just like cache, but with high redundacy and guaranteed entry longevity to achieve durability

        Immutable data store + integrated search engine as a block "chain"
        [] similar to blockchain but with some important differences:
            blocks (key + value in this particular scheme) are NOT secured by chaining them together with cryptographic hashes
            rather, to insert a block it has to "related" to existing blocks and contains the hashes of those blocks
                related in this context is used in the same sense that a search engine index would identify two things as being related
                    internally the search engine will use the relationship to perform searches for arbitrary queries.
                miners mine a block by trying to find the "best" set of blocks which relate to the block being mined.
                validators check each proposal from all miners and give awards to the best.
                    "best" is hard to define succinctly, but the idea is that it effectively organizes the information for search queries.
                        PageRank algorithm is a good starting point.
                        generally, a bad search result will cause a user to issue a repeated query looking for similar information
                        system would A/B test proposals against such traffic patterns to determine the winning miners
                    the difficulty of this problem is scale rather than hashrate;
                        i.e trying to organize zetabytes of information is difficult particualrly if the index is distributed (as here)
*/

use std::time::Duration;

use std::{
    collections::BTreeMap,
    fmt::Debug,
    io::{Read, Write},
    net::SocketAddr,
};

use mio::event::Source;
use mio::{
    event::{Event, Iter},
    net::{TcpListener, TcpStream},
    Events, Interest, Poll, Token,
};

#[cfg(unix)]
use mio::net::{UnixListener, UnixStream};

use std::io::ErrorKind::*;
trait EventLoop {
    type Poller;
    type Event;
    type Events;
    type Iter<'a>: Iterator<Item = &'a Self::Event> + 'a
    where
        <Self as EventLoop>::Event: 'a;
    type Interest: Copy;

    fn new_events_buffer(capacity: usize) -> Self::Events;
    fn new_poller() -> std::io::Result<Self::Poller>;

    fn event_token(event: &Self::Event) -> usize;
    fn event_is_writeable(event: &Self::Event) -> bool;
    fn event_is_readable(event: &Self::Event) -> bool;
    fn events_iter<'a>(events: &'a Self::Events) -> Self::Iter<'a>;

    fn readable_interest() -> Self::Interest;
    fn writeable_interest() -> Self::Interest;
    fn add_readable_to_interest(interest: Self::Interest) -> Self::Interest;
    fn add_writeable_to_interest(interest: Self::Interest) -> Self::Interest;

    fn poll(
        poller: &mut Self::Poller,
        events: &mut Self::Events,
        timeout: Option<Duration>,
    ) -> std::io::Result<()>;
}

trait Registry<C>: EventLoop {
    fn register(
        poller: &Self::Poller,
        connection: &mut C,
        token: usize,
        interest: Self::Interest,
    ) -> std::io::Result<()>;
    fn reregister(
        poller: &Self::Poller,
        connection: &mut C,
        token: usize,
        interest: Self::Interest,
    ) -> std::io::Result<()>;
    fn deregister(poller: &Self::Poller, connection: &mut C) -> std::io::Result<()>;
}

impl<T, C> Registry<C> for T
where
    T: EventLoop<Poller = Poll, Interest = Interest>,
    T: Connector<TcpStream>,
    C: Source,
{
    fn register(
        poller: &Self::Poller,
        connection: &mut C,
        token: usize,
        interest: Self::Interest,
    ) -> std::io::Result<()> {
        poller
            .registry()
            .register(connection, Token(token), interest)
    }

    fn reregister(
        poller: &Self::Poller,
        connection: &mut C,
        token: usize,
        interest: Self::Interest,
    ) -> std::io::Result<()> {
        poller
            .registry()
            .reregister(connection, Token(token), interest)
    }

    fn deregister(poller: &Self::Poller, connection: &mut C) -> std::io::Result<()> {
        poller.registry().deregister(connection)
    }
}

trait ListenerRegistry<C>: Registry<C> + Registry<Self::Listener> {
    type Listener;
}

impl<T> ListenerRegistry<TcpStream> for T
where
    T: Registry<TcpStream>,
    T: EventLoop<Poller = Poll, Interest = Interest>,
    T: Connector<TcpStream>,
{
    type Listener = TcpListener;
}

#[cfg(unix)]
impl<T> ListenerRegistry<UnixStream> for T
where
    T: Registry<UnixStream>,
    T: EventLoop<Poller = Poll, Interest = Interest>,
    T: Connector<UnixStream>,
{
    type Listener = UnixListener;
}

trait Listener<C> {
    type Listener;
    fn bind(addr: SocketAddr) -> std::io::Result<Self::Listener>;
    fn accept(listener: &Self::Listener) -> std::io::Result<(C, SocketAddr)>;
}

impl<T> Listener<TcpStream> for T {
    type Listener = TcpListener;

    #[inline]
    fn bind(addr: SocketAddr) -> std::io::Result<Self::Listener> {
        TcpListener::bind(addr)
    }

    #[inline]
    fn accept(listener: &Self::Listener) -> std::io::Result<(TcpStream, SocketAddr)> {
        listener.accept()
    }
}

#[cfg(unix)]
impl<T> Listener<UnixStream> for T {
    type Listener = UnixListener;

    #[inline]
    fn bind(addr: &SocketAddr) -> std::io::Result<Self::Listener> {
        UnixListener::bind_addr(addr)
    }

    #[inline]
    fn accept(listener: &Self::Listener) -> std::io::Result<(UnixStream, SocketAddr)> {
        listener.accept()
    }
}

trait Connector<C> {
    fn write_on_connection(connection: &mut C, send: &[u8]) -> std::io::Result<usize>
    where
        C: Write;
    fn read_from_connection(connection: &mut C, receive: &mut [u8]) -> std::io::Result<usize>
    where
        C: Read;
}

trait ReadWriteConnectorAdapter {}

impl<R, T> Connector<R> for T
where
    T: ReadWriteConnectorAdapter,
    R: Read + Write,
{
    fn write_on_connection(connection: &mut R, send: &[u8]) -> std::io::Result<usize>
    where
        R: Write,
    {
        connection.write(send)
    }

    fn read_from_connection(connection: &mut R, receive: &mut [u8]) -> std::io::Result<usize>
    where
        R: Read,
    {
        connection.read(receive)
    }
}

trait Connect<C> {
    fn connect(addr: SocketAddr) -> std::io::Result<C>;
}

impl<T> Connect<TcpStream> for T {
    fn connect(addr: SocketAddr) -> std::io::Result<TcpStream> {
        TcpStream::connect(addr)
    }
}

#[cfg(unix)]
impl<T> Connect<UnixStream> for T {
    fn connect(addr: SocketAddr) -> std::io::Result<UnixStream> {
        UnixStream::connect(addr)
    }
}

trait MioEventLoop {}

impl<T> EventLoop for T
where
    T: MioEventLoop,
{
    type Poller = Poll;

    type Event = Event;

    type Events = Events;

    type Iter<'a> = Iter<'a>
    where
        <Self as EventLoop>::Event: 'a;

    type Interest = Interest;

    #[inline]
    fn new_events_buffer(capacity: usize) -> Self::Events {
        Events::with_capacity(capacity)
    }

    #[inline]
    fn new_poller() -> std::io::Result<Self::Poller> {
        Poll::new()
    }

    #[inline]
    fn event_token(event: &Self::Event) -> usize {
        event.token().0
    }

    #[inline]
    fn event_is_writeable(event: &Self::Event) -> bool {
        event.is_writable()
    }

    #[inline]
    fn event_is_readable(event: &Self::Event) -> bool {
        event.is_readable()
    }

    #[inline]
    fn events_iter<'a>(events: &'a Self::Events) -> Self::Iter<'a> {
        events.iter()
    }

    #[inline]
    fn poll(
        poller: &mut Self::Poller,
        events: &mut Self::Events,
        timeout: Option<Duration>,
    ) -> std::io::Result<()> {
        poller.poll(events, timeout)
    }

    #[inline]
    fn readable_interest() -> Self::Interest {
        Interest::READABLE
    }

    #[inline]
    fn writeable_interest() -> Self::Interest {
        Interest::WRITABLE
    }

    #[inline]
    fn add_readable_to_interest(interest: Self::Interest) -> Self::Interest {
        interest.add(Interest::READABLE)
    }

    #[inline]
    fn add_writeable_to_interest(interest: Self::Interest) -> Self::Interest {
        interest.add(Interest::WRITABLE)
    }
}

// TODO although low level event loop code for TCP / Unix sockets is normally pretty ugly and control flow heavy
//        figure out a way to make it not so ugly if possible

trait Server<C>:
    ListenerRegistry<C>
    + Listener<C, Listener = <Self as ListenerRegistry<C>>::Listener>
    + Connector<C>
    + EventLoop<Poller = Poll, Interest = Interest>
    + ReadWriteConnectorAdapter
    + Sized
where
    C: Read + Write + Debug,
    <Self as Listener<C>>::Listener: Source,
    <Self as EventLoop>::Event: Debug,
{
    fn server<const SERVER: usize>(
        addr: SocketAddr,
        event_buffer_capacity: usize,
        receive: &mut [u8; 4096],
        send: &'static [u8],
    ) -> std::io::Result<()> {
        let mut client_token = SERVER;
        let mut new_client_token = || {
            client_token = std::cmp::max(client_token, SERVER);
            client_token = client_token.wrapping_add(1);
            client_token
        };
        let mut poller = Self::new_poller()?;
        let mut events = Self::new_events_buffer(event_buffer_capacity);
        let mut server = Self::bind(addr)?;
        let interest = Self::readable_interest();
        <Self as Registry<<Self as ListenerRegistry<C>>::Listener>>::register(
            &poller,
            &mut server,
            SERVER,
            interest,
        )?;
        let mut connections = BTreeMap::new();
        let mut written = false;
        let mut read = false;
        loop {
            if let Err(err) = Self::poll(&mut poller, &mut events, None) {
                if Interrupted == err.kind() {
                    continue;
                }
                return Err(err);
            }
            for event in Self::events_iter(&events) {
                let mut bytes_read: usize = 0;
                let token = Self::event_token(&event);
                if SERVER == token {
                    loop {
                        let (mut connection, _address) = match Self::accept(&server) {
                            Ok((connection, address)) => (connection, address),
                            Err(e) if WouldBlock == e.kind() => break,
                            Err(e) => return Err(e).unwrap(),
                        };
                        let token = new_client_token();
                        let interest = Self::add_readable_to_interest(Self::writeable_interest());
                        <Self as Registry<C>>::register(&poller, &mut connection, token, interest)?;
                        connections.insert(token, connection);
                    }
                }
                {
                    let mut done = false;
                    if let Some(mut connection) = connections.get_mut(&token) {
                        if !read && Self::event_is_readable(&event) {
                            let mut connection_closed = false;
                            loop {
                                match Self::read_from_connection(
                                    &mut connection,
                                    &mut receive[bytes_read..],
                                ) {
                                    Ok(0) => {
                                        connection_closed = true;
                                        break;
                                    }
                                    Ok(n) => {
                                        assert!(n + bytes_read <= receive.len());
                                        bytes_read += n;
                                        if bytes_read >= send.len() {
                                            read = true;
                                            break;
                                        }
                                        continue;
                                    }
                                    Err(ref err) if WouldBlock == err.kind() => {
                                        break;
                                    }
                                    Err(ref err) if Interrupted == err.kind() => {
                                        continue;
                                    }
                                    Err(err) => {
                                        return Err(err);
                                    }
                                }
                            }
                            if connection_closed {
                                done = true;
                            }
                        }
                        loop {
                            if read && Self::event_is_writeable(&event) {
                                match Self::write_on_connection(connection, send) {
                                    Ok(n) if n < send.len() => {
                                        return Err::<(), std::io::Error>(WriteZero.into());
                                    }
                                    Ok(n) => {
                                        written = true;
                                        break;
                                    }
                                    Err(ref err) if WouldBlock == err.kind() => {
                                        break;
                                    }
                                    Err(ref err) if Interrupted == err.kind() => {
                                        continue;
                                    }
                                    Err(err) => {
                                        return Err(err);
                                    }
                                }
                            }
                        }
                    }
                    if written {
                        done = true;
                    }
                    if done {
                        if let Some(mut connection) = connections.remove(&token) {
                            Self::deregister(&poller, &mut connection)?
                        }
                        return Ok(());
                    }
                }
            }
        }
    }
}

trait Client<C>:
    Registry<C> + Connect<C> + Connector<C> + EventLoop<Poller = Poll, Interest = Interest>
{
    fn client<const CLIENT: usize>(
        addr: SocketAddr,
        event_buffer_capacity: usize,
        receive: &mut [u8; 4096],
        send: &'static [u8],
    ) -> std::io::Result<()>
    where
        C: Read + Write,
        <Self as EventLoop>::Event: Debug,
    {
        let mut poller = Self::new_poller()?;
        let mut events = Self::new_events_buffer(event_buffer_capacity);
        let mut connection: C = Self::connect(addr)?;
        let interest = Self::add_readable_to_interest(Self::writeable_interest());
        Self::register(&poller, &mut connection, CLIENT, interest)?;
        let mut written = false;
        let mut read = false;

        loop {
            if let Err(err) = Self::poll(&mut poller, &mut events, None) {
                if Interrupted == err.kind() {
                    continue;
                }
                return Err(err);
            }
            let events_iter = Self::events_iter(&events);
            for event in events_iter {
                let mut bytes_read: usize = 0;
                if Self::event_token(&event) == CLIENT {
                    if !written && Self::event_is_writeable(&event) {
                        match Self::write_on_connection(&mut connection, send) {
                            Ok(n) if n < send.len() => {
                                return Err(WriteZero.into());
                            }
                            Ok(_n) => {
                                written = true;
                                let interest = Self::readable_interest();
                                Self::reregister(&poller, &mut connection, CLIENT, interest)?;
                            }
                            Err(ref err) if WouldBlock == err.kind() => {}
                            Err(ref err) if Interrupted == err.kind() => {
                                continue;
                            }
                            Err(err) => {
                                return Err(err);
                            }
                        }
                    }
                    if written && Self::event_is_readable(&event) {
                        loop {
                            match Self::read_from_connection(
                                &mut connection,
                                &mut receive[bytes_read..],
                            ) {
                                Ok(0) => {
                                    break;
                                }
                                Ok(n) => {
                                    bytes_read += n;
                                    if bytes_read >= send.len() {
                                        read = true;
                                        break;
                                    }
                                    continue;
                                }
                                Err(ref err) if WouldBlock == err.kind() => {
                                    break;
                                }
                                Err(ref err) if Interrupted == err.kind() => {
                                    continue;
                                }
                                Err(err) => {
                                    return Err(err);
                                }
                            }
                        }
                    }
                }

                if read {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::atomic::{AtomicU32, Ordering::*},
        thread,
    };

    use mio::net::TcpStream;

    use crate::{Client, MioEventLoop, ReadWriteConnectorAdapter, Server};

    static ADDRESS: AtomicU32 = AtomicU32::new(1 + (49152 << 16));

    fn new_loopback_address() -> SocketAddr {
        ADDRESS.fetch_max(1 + (49152 << 16), SeqCst);
        let mut a = ADDRESS.fetch_add(1, SeqCst);
        let y = (a & 255) as u8;
        a = a >> 8;
        let x = (a & 255) as u8;
        a = a >> 8;
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, x, y)), a as u16)
    }

    #[test]
    fn it_works() {
        static SEND_TO_CLIENT: &'static [u8] = b"send to client\n";
        static SEND_TO_SERVER: &'static [u8] = b"send to server\n";
        let mut receive_from_client: [u8; 4096] = [0; 4096];
        let mut receive_from_server: [u8; 4096] = [0; 4096];

        let addr = new_loopback_address();

        struct TestClient;

        // marker traits that cause Client<TcpStream> to be implementable automatically
        impl MioEventLoop for TestClient {}
        impl ReadWriteConnectorAdapter for TestClient {}
        impl Client<TcpStream> for TestClient {}

        struct TestServer;
        impl MioEventLoop for TestServer {}
        impl ReadWriteConnectorAdapter for TestServer {}
        impl Server<TcpStream> for TestServer {}

        thread::scope(|s| {
            s.spawn(|| {
                TestServer::server::<1>(addr, 128, &mut receive_from_client, SEND_TO_CLIENT)
            });
            s.spawn(|| {
                TestClient::client::<2>(addr, 128, &mut receive_from_server, SEND_TO_SERVER)
            });
        });

        assert_eq!(&receive_from_server[..SEND_TO_CLIENT.len()], SEND_TO_CLIENT);
        assert_eq!(&receive_from_client[..SEND_TO_SERVER.len()], SEND_TO_SERVER)
    }
}
