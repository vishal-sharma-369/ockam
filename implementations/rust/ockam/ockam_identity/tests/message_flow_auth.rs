use core::time::Duration;
use ockam_core::compat::net::SocketAddr;
use ockam_core::sessions::{SessionId, Sessions};
use ockam_core::{route, Address, AllowAll, Result, Route};
use ockam_identity::authenticated_storage::mem::InMemoryStorage;
use ockam_identity::{
    Identity, SecureChannelListenerTrustOptions, SecureChannelTrustOptions, TrustEveryonePolicy,
};
use ockam_node::Context;
use ockam_transport_tcp::{TcpConnectionTrustOptions, TcpListenerTrustOptions, TcpTransport};
use ockam_vault::Vault;
use rand::random;

async fn check_message_flow(ctx: &Context, route: Route, should_pass: bool) -> Result<()> {
    let address = Address::random_local();
    let mut receiving_ctx = ctx
        .new_detached(address.clone(), AllowAll, AllowAll)
        .await?;

    let msg: [u8; 4] = random();
    let msg = hex::encode(&msg);
    ctx.send(route![route, address], msg.clone()).await?;

    if should_pass {
        let msg_received = receiving_ctx.receive::<String>().await?.take().body();
        assert_eq!(msg_received, msg);
    } else {
        let res = receiving_ctx.receive_timeout::<String>(1).await;
        assert!(res.is_err(), "Messages should not pass for given route");
    }

    Ok(())
}

struct TcpListenerInfo {
    tcp: TcpTransport,
    socket_addr: SocketAddr,
    session: Option<(Sessions, SessionId)>,
}

impl TcpListenerInfo {
    fn get_connection(&self) -> Address {
        self.tcp
            .registry()
            .get_all_sender_workers()
            .first()
            .unwrap()
            .clone()
    }
}

async fn create_tcp_listener(ctx: &Context, with_session: bool) -> Result<TcpListenerInfo> {
    let tcp = TcpTransport::create(ctx).await?;
    let (socket_addr, session) = if with_session {
        let sessions = Sessions::default();
        let session_id = sessions.generate_session_id();
        let trust_options = TcpListenerTrustOptions::new().with_session(&sessions, &session_id);
        let (socket_addr, _) = tcp.listen_trust("127.0.0.1:0", trust_options).await?;
        (socket_addr, Some((sessions, session_id)))
    } else {
        let (socket_addr, _) = tcp.listen("127.0.0.1:0").await?;
        (socket_addr, None)
    };

    let info = TcpListenerInfo {
        tcp,
        socket_addr,
        session,
    };

    Ok(info)
}

struct TcpConnectionInfo {
    address: Address,
    session: Option<(Sessions, SessionId)>,
}

async fn create_connection(
    ctx: &Context,
    socket_addr: &SocketAddr,
    with_session: bool,
) -> Result<TcpConnectionInfo> {
    let tcp = TcpTransport::create(ctx).await?;
    let (address, session) = if with_session {
        let sessions = Sessions::default();
        let session_id = sessions.generate_session_id();
        let trust_options = TcpConnectionTrustOptions::new().with_session(&sessions, &session_id);
        let address = tcp
            .connect_trust(socket_addr.to_string(), trust_options)
            .await?;
        (address, Some((sessions, session_id)))
    } else {
        let address = tcp.connect(socket_addr.to_string()).await?;
        (address, None)
    };

    let info = TcpConnectionInfo { address, session };

    Ok(info)
}

struct SecureChannelListenerInfo {
    identity: Identity<Vault, InMemoryStorage>,
}

impl SecureChannelListenerInfo {
    fn get_channel(&self) -> Address {
        self.identity
            .secure_channel_registry()
            .get_channel_list()
            .first()
            .unwrap()
            .encryptor_messaging_address()
            .clone()
    }
}

async fn create_secure_channel_listener(
    ctx: &Context,
    session: &Option<(Sessions, SessionId)>,
) -> Result<SecureChannelListenerInfo> {
    let identity = Identity::create(ctx, &Vault::create()).await?;

    if let Some((sessions, session_id)) = session {
        let trust_options =
            SecureChannelListenerTrustOptions::new().with_session(sessions, session_id);
        identity
            .create_secure_channel_listener_trust("listener", trust_options)
            .await?;
    } else {
        identity
            .create_secure_channel_listener("listener", TrustEveryonePolicy)
            .await?;
    }

    let info = SecureChannelListenerInfo { identity };

    Ok(info)
}

struct SecureChannelInfo {
    identity: Identity<Vault, InMemoryStorage>,
    address: Address,
}

async fn create_secure_channel(
    ctx: &Context,
    connection: &TcpConnectionInfo,
) -> Result<SecureChannelInfo> {
    let identity = Identity::create(ctx, &Vault::create()).await?;

    let address = if let Some((sessions, session_id)) = &connection.session {
        let trust_options =
            SecureChannelTrustOptions::new().with_ciphertext_session(sessions, session_id);
        identity
            .create_secure_channel_trust(
                route![connection.address.clone(), "listener"],
                trust_options,
            )
            .await?
    } else {
        identity
            .create_secure_channel(
                route![connection.address.clone(), "listener"],
                TrustEveryonePolicy,
            )
            .await?
    };

    let info = SecureChannelInfo { identity, address };

    Ok(info)
}

#[allow(non_snake_case)]
#[ockam_macros::test]
async fn sessions__secure_channel_over_tcp_without_session__should_pass_messages(
    ctx: &mut Context,
) -> Result<()> {
    let bob_tcp_info = create_tcp_listener(ctx, false).await?;

    let bob_listener_info = create_secure_channel_listener(ctx, &bob_tcp_info.session).await?;

    let connection_to_bob = create_connection(ctx, &bob_tcp_info.socket_addr, false).await?;
    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let connection_to_alice = bob_tcp_info.get_connection();

    check_message_flow(ctx, route![connection_to_bob.address.clone()], true).await?;
    check_message_flow(ctx, route![connection_to_alice.clone()], true).await?;

    let channel_to_bob = create_secure_channel(ctx, &connection_to_bob).await?;
    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let channel_to_alice = bob_listener_info.get_channel();

    check_message_flow(ctx, route![channel_to_bob.address.clone()], true).await?;
    check_message_flow(ctx, route![channel_to_alice.clone()], true).await?;

    ctx.stop().await
}

#[allow(non_snake_case)]
#[ockam_macros::test]
async fn sessions__secure_channel_over_tcp_with_alice_session__should_not_pass_messages(
    ctx: &mut Context,
) -> Result<()> {
    let bob_tcp_info = create_tcp_listener(ctx, false).await?;

    let connection_to_bob = create_connection(ctx, &bob_tcp_info.socket_addr, true).await?;

    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let connection_to_alice = bob_tcp_info.get_connection();

    check_message_flow(ctx, route![connection_to_bob.address.clone()], true).await?;
    check_message_flow(ctx, route![connection_to_alice.clone()], false).await?;

    let bob_listener_info = create_secure_channel_listener(ctx, &bob_tcp_info.session).await?;

    let channel_to_bob = create_secure_channel(ctx, &connection_to_bob).await?;
    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let channel_to_alice = bob_listener_info.get_channel();

    check_message_flow(ctx, route![channel_to_bob.address.clone()], true).await?;
    check_message_flow(ctx, route![channel_to_alice.clone()], true).await?;

    let res = channel_to_bob
        .identity
        .create_secure_channel_extended(
            route![connection_to_bob.address.clone(), "listener"],
            TrustEveryonePolicy,
            Duration::from_secs(1),
        )
        .await;
    assert!(
        res.is_err(),
        "We can only create 1 secure channel with that connection"
    );

    ctx.stop().await
}

#[allow(non_snake_case)]
#[ockam_macros::test]
async fn sessions__secure_channel_over_tcp_with_bob_session__should_not_pass_messages(
    ctx: &mut Context,
) -> Result<()> {
    let bob_tcp_info = create_tcp_listener(&ctx, true).await?;

    let connection_to_bob = create_connection(ctx, &bob_tcp_info.socket_addr, false).await?;
    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let connection_to_alice = bob_tcp_info.get_connection();

    check_message_flow(ctx, route![connection_to_bob.address.clone()], false).await?;
    check_message_flow(ctx, route![connection_to_alice.clone()], true).await?;

    let bob_listener_info = create_secure_channel_listener(ctx, &bob_tcp_info.session).await?;

    let channel_to_bob = create_secure_channel(ctx, &connection_to_bob).await?;
    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let channel_to_alice = bob_listener_info.get_channel();

    check_message_flow(ctx, route![channel_to_bob.address.clone()], true).await?;
    check_message_flow(ctx, route![channel_to_alice.clone()], true).await?;

    let res = channel_to_bob
        .identity
        .create_secure_channel_extended(
            route![connection_to_bob.address.clone(), "listener"],
            TrustEveryonePolicy,
            Duration::from_secs(1),
        )
        .await;
    assert!(
        res.is_err(),
        "We can only create 1 secure channel with that connection"
    );

    ctx.stop().await
}

#[allow(non_snake_case)]
#[ockam_macros::test]
async fn sessions__secure_channel_over_tcp_with_both_sides_session__should_not_pass_messages(
    ctx: &mut Context,
) -> Result<()> {
    let bob_tcp_info = create_tcp_listener(ctx, true).await?;

    let connection_to_bob = create_connection(ctx, &bob_tcp_info.socket_addr, true).await?;
    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let connection_to_alice = bob_tcp_info.get_connection();

    check_message_flow(ctx, route![connection_to_bob.address.clone()], false).await?;
    check_message_flow(ctx, route![connection_to_alice.clone()], false).await?;

    let bob_listener_info = create_secure_channel_listener(ctx, &bob_tcp_info.session).await?;

    let channel_to_bob = create_secure_channel(ctx, &connection_to_bob).await?;
    ctx.sleep(Duration::from_millis(50)).await; // Wait for workers to add themselves to the registry
    let channel_to_alice = bob_listener_info.get_channel();

    check_message_flow(ctx, route![channel_to_bob.address.clone()], true).await?;
    check_message_flow(ctx, route![channel_to_alice.clone()], true).await?;

    let res = channel_to_bob
        .identity
        .create_secure_channel_extended(
            route![connection_to_bob.address.clone(), "listener"],
            TrustEveryonePolicy,
            Duration::from_secs(1),
        )
        .await;
    assert!(
        res.is_err(),
        "We can only create 1 secure channel with that connection"
    );

    ctx.stop().await
}
