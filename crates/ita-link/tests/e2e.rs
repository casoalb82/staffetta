//! End to end: two fake networks on loopback, real sockets, the real runner.
//!
//! Safety: the fake networks listen on 127.0.0.1 with OS-chosen ports, the
//! config validator refuses plain-text anywhere else, and the test asserts the
//! ports it connects to are the ones its own servers own.

use ita_link::config::Config;
use ita_link::runner;
use itl_testnet::{Client, TestNet};
use std::time::Duration;
use tokio::sync::mpsc;

fn cfg(dir: &std::path::Path, a: &TestNet, b: &TestNet) -> Config {
    assert!(a.addr.ip().is_loopback() && b.addr.ip().is_loopback());
    let toml = format!(
        r#"
        service_nick = "ItaLink"
        default_lang = "en"
        state_dir = "{}"
        [[network]]
        id = "chatnet"
        label = "ChatNet"
        host = "127.0.0.1"
        port = {}
        tls = false
        [[network]]
        id = "azzurra"
        label = "Azzurra"
        host = "127.0.0.1"
        port = {}
        tls = false
        "#,
        dir.display(),
        a.port(),
        b.port()
    );
    let c: Config = toml::from_str(&toml).unwrap();
    c.validate().unwrap();
    c
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ita-link-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn until(secs: u64, f: impl Fn() -> bool) -> bool {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_networks_one_conversation() {
    let net_a = TestNet::start("chatnet.test").await;
    let net_b = TestNet::start("azzurra.test").await;
    let dir = tmpdir("flow");
    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    let c = cfg(&dir, &net_a, &net_b);
    let svc = tokio::spawn(runner::run(c, stop_rx));

    // The service connects to both networks (and nobody else exists yet).
    assert!(until(10, || net_a.has_user("ItaLink") && net_b.has_user("ItaLink")).await, "service did not connect");

    // Real people on both sides.
    let mut op_a = Client::connect(&net_a, "opA").await;
    let mut alice = Client::connect(&net_a, "alice").await;
    op_a.join("#italy").await;
    alice.join("#italy").await;
    let mut op_b = Client::connect(&net_b, "opB").await;
    let mut bob = Client::connect(&net_b, "bob").await;
    op_b.join("#italia").await;
    bob.join("#italia").await;

    // Consent step 1: each channel's operator invites the service in.
    op_a.send("INVITE ItaLink #italy").await;
    op_b.send("INVITE ItaLink #italia").await;
    assert!(until(10, || net_a.is_in("#italy", "ItaLink") && net_b.is_in("#italia", "ItaLink")).await);
    tokio::time::sleep(Duration::from_millis(500)).await; // let it learn who is in the channels

    // Consent step 2: both operators agree, then each goes LIVE by itself.
    op_a.say("ItaLink", "LINK #italy azzurra #italia").await;
    let note = op_b.wait_for(|l| l.contains("NOTICE #italia") && l.contains("ACCEPT"), 5).await;
    assert!(note.is_some(), "the other channel is told how to accept");
    op_b.say("ItaLink", "ACCEPT #italia chatnet #italy").await;
    op_a.say("ItaLink", "SET #italy min-presence 0").await;
    op_b.say("ItaLink", "SET #italia min-presence 0").await;

    // Preview: nothing crosses yet.
    alice.say("#italy", "ciao prima del live").await;
    assert!(bob.silent_for(|l| l.contains("ciao prima del live"), 600).await, "preview must not leak");

    op_a.say("ItaLink", "LIVE #italy").await;
    op_b.say("ItaLink", "LIVE #italia").await;
    op_b.wait_for(|l| l.contains("now LIVE"), 5).await.expect("live confirmed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The conversation flows, with a tagged puppet.
    alice.say("#italy", "ciao dall'Italia!").await;
    let got = bob.wait_for(|l| l.contains("PRIVMSG #italia") && l.contains("ciao dall'Italia!"), 8).await;
    let got = got.expect("bob hears alice");
    assert!(got.starts_with(":alice|cha!cha-"), "{got}");
    assert!(net_b.is_in("#italia", "alice|cha"));
    let ident = net_b.user_ident("alice|cha").unwrap();
    assert!(ident.starts_with("cha-") && ident.len() <= 10, "{ident}");
    assert!(net_b.user_realname("alice|cha").unwrap().contains("relayed"));

    bob.say("#italia", "ciao da Azzurra").await;
    let got = alice.wait_for(|l| l.contains("PRIVMSG #italy") && l.contains("ciao da Azzurra"), 8).await;
    assert!(got.expect("alice hears bob").starts_with(":bob|azz!azz-"));

    // The #italia operator bans the puppet the normal IRC way: it sticks.
    net_b.ban_and_kick("#italia", "alice|cha");
    tokio::time::sleep(Duration::from_millis(400)).await;
    alice.say("#italy", "sono ancora qui?").await;
    assert!(bob.silent_for(|l| l.contains("sono ancora qui?"), 1200).await, "banned person must stay out");
    assert!(!net_b.is_in("#italia", "alice|cha"));

    // The language is per channel: #italia switches to Italian.
    op_b.say("ItaLink", "SET #italia lang it").await;
    op_b.say("ItaLink", "STATUS #italia").await;
    assert!(op_b.wait_for(|l| l.contains("righe in entrata"), 5).await.is_some(), "Italian status");

    // The owner can see it in the audit trail.
    op_b.say("ItaLink", "AUDIT #italia 10").await;
    assert!(
        op_b.wait_for(|l| l.contains("NOTICE") && l.contains("was kicked: kept out"), 5).await.is_some(),
        "the audit trail records the kick"
    );

    // Instant stop.
    op_b.say("ItaLink", "UNLINK #italia").await;
    op_b.wait_for(|l| l.contains("non è più collegato"), 5).await.expect("unlink confirmed");
    bob.say("#italia", "silenzio?").await;
    assert!(alice.silent_for(|l| l.contains("silenzio?"), 800).await);

    stop_tx.send(()).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), svc).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_a_network_drop_and_a_restart() {
    let net_a = TestNet::start("chatnet.test").await;
    let net_b = TestNet::start("azzurra.test").await;
    let dir = tmpdir("restart");
    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    let svc = tokio::spawn(runner::run(cfg(&dir, &net_a, &net_b), stop_rx));
    assert!(until(10, || net_a.has_user("ItaLink") && net_b.has_user("ItaLink")).await);

    let mut op_a = Client::connect(&net_a, "opA").await;
    let mut alice = Client::connect(&net_a, "alice").await;
    op_a.join("#italy").await;
    alice.join("#italy").await;
    let mut op_b = Client::connect(&net_b, "opB").await;
    let mut bob = Client::connect(&net_b, "bob").await;
    op_b.join("#italia").await;
    bob.join("#italia").await;
    op_a.send("INVITE ItaLink #italy").await;
    op_b.send("INVITE ItaLink #italia").await;
    assert!(until(10, || net_a.is_in("#italy", "ItaLink") && net_b.is_in("#italia", "ItaLink")).await);
    tokio::time::sleep(Duration::from_millis(500)).await;
    op_a.say("ItaLink", "LINK #italy azzurra #italia").await;
    op_b.wait_for(|l| l.contains("ACCEPT"), 5).await.unwrap();
    op_b.say("ItaLink", "ACCEPT #italia chatnet #italy").await;
    for (c, ch, cmd) in [(&mut op_a, "#italy", "min-presence 0"), (&mut op_b, "#italia", "min-presence 0")] {
        c.say("ItaLink", &format!("SET {ch} {cmd}")).await;
    }
    op_a.say("ItaLink", "LIVE #italy").await;
    op_b.say("ItaLink", "LIVE #italia").await;
    op_b.wait_for(|l| l.contains("now LIVE"), 5).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 1) The service's connection to one network dies: it must come back alone.
    net_a.disconnect("ItaLink");
    assert!(until(5, || !net_a.has_user("ItaLink")).await, "connection dropped");
    assert!(until(20, || net_a.is_in("#italy", "ItaLink")).await, "service reconnected and rejoined");
    tokio::time::sleep(Duration::from_millis(700)).await;
    alice.say("#italy", "dopo la caduta").await;
    assert!(bob.wait_for(|l| l.contains("dopo la caduta"), 8).await.is_some(), "flow resumed");

    // 2) The whole service restarts: links, rules and channels come back from disk.
    stop_tx.send(()).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), svc).await;
    assert!(dir.join("state.json").exists());
    assert!(dir.join("audit.jsonl").exists());
    let (stop_tx2, stop_rx2) = mpsc::unbounded_channel();
    let svc2 = tokio::spawn(runner::run(cfg(&dir, &net_a, &net_b), stop_rx2));
    assert!(until(20, || net_a.is_in("#italy", "ItaLink") && net_b.is_in("#italia", "ItaLink")).await, "rejoined after restart");
    tokio::time::sleep(Duration::from_millis(700)).await;
    bob.say("#italia", "dopo il riavvio").await;
    assert!(alice.wait_for(|l| l.contains("dopo il riavvio"), 8).await.is_some(), "link survived the restart");

    stop_tx2.send(()).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), svc2).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accounts_anchor_identity_and_gate_entry() {
    use itl_core::model::{NetId, SeenUser};
    let net_a = TestNet::start("chatnet.test").await;
    let net_b = TestNet::start("azzurra.test").await;
    let dir = tmpdir("accounts");
    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    let svc = tokio::spawn(runner::run(cfg(&dir, &net_a, &net_b), stop_rx));
    assert!(until(10, || net_a.has_user("ItaLink") && net_b.has_user("ItaLink")).await);

    let mut op_a = Client::connect(&net_a, "opA").await;
    let mut alice = Client::connect(&net_a, "alice").await;
    let mut anon = Client::connect(&net_a, "anon").await;
    net_a.set_account("alice", "AliceAcct"); // logged in to services
    op_a.join("#italy").await;
    alice.join("#italy").await;
    anon.join("#italy").await;
    let mut op_b = Client::connect(&net_b, "opB").await;
    let mut bob = Client::connect(&net_b, "bob").await;
    op_b.join("#italia").await;
    bob.join("#italia").await;
    op_a.send("INVITE ItaLink #italy").await;
    op_b.send("INVITE ItaLink #italia").await;
    assert!(until(10, || net_a.is_in("#italy", "ItaLink") && net_b.is_in("#italia", "ItaLink")).await);
    tokio::time::sleep(Duration::from_millis(500)).await;
    op_a.say("ItaLink", "LINK #italy azzurra #italia").await;
    op_b.wait_for(|l| l.contains("ACCEPT"), 5).await.unwrap();
    op_b.say("ItaLink", "ACCEPT #italia chatnet #italy").await;
    for (c, ch) in [(&mut op_a, "#italy"), (&mut op_b, "#italia")] {
        c.say("ItaLink", &format!("SET {ch} min-presence 0")).await;
        c.say("ItaLink", &format!("SET {ch} lang en")).await;
    }
    op_b.say("ItaLink", "SET #italia require-account on").await;
    op_a.say("ItaLink", "LIVE #italy").await;
    op_b.say("ItaLink", "LIVE #italia").await;
    op_b.wait_for(|l| l.contains("now LIVE"), 5).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The account (learned through WHOX) lets alice in; the anonymous user is kept out.
    anon.say("#italy", "sono anonimo").await;
    assert!(bob.silent_for(|l| l.contains("sono anonimo"), 800).await, "no account, no entry");
    alice.say("#italy", "ho un account").await;
    assert!(bob.wait_for(|l| l.contains("ho un account"), 8).await.is_some());

    // And her ident is derived from the ACCOUNT, not the nick.
    let want = itl_core::face::ident_for(&NetId::new("chatnet"), &SeenUser::new("alice", "x", "y").with_account("AliceAcct"));
    assert_eq!(net_b.user_ident("alice|cha").unwrap(), want);

    stop_tx.send(()).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), svc).await;
    let _ = std::fs::remove_dir_all(&dir);
}
