//! Every sentence the service says to a person, in each supported language.
//!
//! Adding a language means adding one `match` arm per message here: nothing
//! else in the code has to change.

use crate::policy::Lang;

pub enum Msg<'a> {
    NotInChannel {
        chan: &'a str,
        svc: &'a str,
    },
    NotSeen {
        chan: &'a str,
    },
    NotOp {
        chan: &'a str,
    },
    NeedAccount,
    NotLinkedYet {
        chan: &'a str,
    },
    SameNetwork,
    TheirServiceMissing {
        chan: &'a str,
        net: &'a str,
        svc: &'a str,
    },
    AlreadyLinked,
    BothInLinks,
    OnePerNet {
        net: &'a str,
    },
    AskNotice {
        mine_chan: &'a str,
        mine_net: &'a str,
        other_chan: &'a str,
        svc: &'a str,
    },
    RequestSent {
        other: &'a str,
    },
    NoPending {
        mine: &'a str,
        net: &'a str,
        chan: &'a str,
    },
    Accepted,
    NoSuchRequest,
    RequestRemoved,
    Admitted {
        svc: &'a str,
        chan: &'a str,
    },
    AdmittedOther {
        chan: &'a str,
        label: &'a str,
    },
    LeftNotice {
        chan: &'a str,
        net: &'a str,
    },
    Unlinked {
        chan: &'a str,
    },
    NotLinked {
        chan: &'a str,
    },
    Paused {
        chan: &'a str,
    },
    Resumed {
        chan: &'a str,
    },
    ModeLive {
        chan: &'a str,
    },
    ModePreview {
        chan: &'a str,
    },
    ModeOff {
        chan: &'a str,
    },
    SetOk {
        chan: &'a str,
        key: &'a str,
        value: &'a str,
    },
    Blocked {
        chan: &'a str,
        mask: &'a str,
    },
    Unblocked {
        chan: &'a str,
        mask: &'a str,
    },
    NotBlocked {
        mask: &'a str,
    },
    BlockFull,
    BadMask,
    Muted {
        chan: &'a str,
        other: &'a str,
    },
    Unmuted {
        chan: &'a str,
        other: &'a str,
    },
    SampleNone,
    SampleHeader {
        n: usize,
        chan: &'a str,
    },
    AuditNone,
    NetDown {
        label: &'a str,
    },
}

impl Msg<'_> {
    pub fn render(&self, lang: Lang) -> String {
        use Msg::*;
        let it = lang == Lang::It;
        match self {
            NotInChannel { chan, svc } => {
                if it {
                    format!("Non sono in {chan}. Un operatore del canale deve prima fare /INVITE {svc} nel canale.")
                } else {
                    format!(
                        "I am not in {chan}. A channel operator must /INVITE {svc} into it first."
                    )
                }
            }
            NotSeen { chan } => {
                if it {
                    format!("Non ti vedo in {chan}.")
                } else {
                    format!("I do not see you in {chan}.")
                }
            }
            NotOp { chan } => {
                if it {
                    format!("Solo gli operatori di {chan} possono farlo.")
                } else {
                    format!("Only channel operators of {chan} can do this.")
                }
            }
            NeedAccount => {
                if it {
                    "Devi essere identificato ai servizi per gestire un link.".into()
                } else {
                    "You need to be logged in to services to manage a link.".into()
                }
            }
            NotLinkedYet { chan } => {
                if it {
                    format!("{chan} non è ancora collegato a niente. Usa prima LINK.")
                } else {
                    format!("{chan} is not linked to anything yet. Use LINK first.")
                }
            }
            SameNetwork => {
                if it {
                    "I due canali sono sulla stessa rete: possono già parlarsi.".into()
                } else {
                    "Both channels are on the same network - they can already talk to each other."
                        .into()
                }
            }
            TheirServiceMissing { chan, net, svc } => {
                if it {
                    format!("Non sono in {chan} su {net}. I suoi operatori devono prima fare /INVITE {svc} lì.")
                } else {
                    format!("I am not in {chan} on {net}. Its operators must /INVITE {svc} there first.")
                }
            }
            AlreadyLinked => {
                if it {
                    "Questi due canali sono già collegati.".into()
                } else {
                    "These two channels are already linked.".into()
                }
            }
            BothInLinks => {
                if it {
                    "Entrambi i canali fanno già parte di un link. Fai prima UNLINK su uno dei due."
                        .into()
                } else {
                    "Both channels already belong to a link. UNLINK one of them first.".into()
                }
            }
            OnePerNet { net } => {
                if it {
                    format!("Quel link ha già un canale su {net}. Un solo canale per rete.")
                } else {
                    format!("That link already has a channel on {net}. One channel per network.")
                }
            }
            AskNotice {
                mine_chan,
                mine_net,
                other_chan,
                svc,
            } => {
                if it {
                    format!("{mine_chan} su {mine_net} chiede di collegarsi con questo canale. Operatori: /msg {svc} ACCEPT {other_chan} {mine_net} {mine_chan} (oppure DENY).")
                } else {
                    format!("{mine_chan} on {mine_net} asks to link with this channel. Operators here: /msg {svc} ACCEPT {other_chan} {mine_net} {mine_chan}  (or DENY).")
                }
            }
            RequestSent { other } => {
                if it {
                    format!("Richiesta inviata. {other} deve accettare prima che succeda qualcosa; ogni link parte in PREVIEW: non si condivide nulla finché ogni lato non dice LIVE.")
                } else {
                    format!("Request sent. {other} must accept before anything happens; every link starts in PREVIEW, nothing is shared until each side says LIVE.")
                }
            }
            NoPending { mine, net, chan } => {
                if it {
                    format!("Nessuna richiesta in sospeso tra {mine} e {net} {chan}.")
                } else {
                    format!("No pending request between {mine} and {net} {chan}.")
                }
            }
            Accepted => {
                if it {
                    "Accettato.".into()
                } else {
                    "Accepted.".into()
                }
            }
            NoSuchRequest => {
                if it {
                    "Nessuna richiesta in sospeso di questo tipo.".into()
                } else {
                    "No such pending request.".into()
                }
            }
            RequestRemoved => {
                if it {
                    "Richiesta rimossa.".into()
                } else {
                    "Request removed.".into()
                }
            }
            Admitted { svc, chan } => {
                if it {
                    format!("Questo canale ora è collegato (modalità PREVIEW: per ora non si condivide nulla). Operatori: /msg {svc} STATUS {chan}  |  LIVE {chan}  quando siete pronti.")
                } else {
                    format!("This channel is now linked (PREVIEW mode: nothing is shared yet). Operators: /msg {svc} STATUS {chan}  |  LIVE {chan}  when you are ready.")
                }
            }
            AdmittedOther { chan, label } => {
                if it {
                    format!("{chan} su {label} è entrato in questo link (parte in PREVIEW; non si condivide nulla finché i suoi operatori non dicono LIVE).")
                } else {
                    format!("{chan} on {label} joined this link (it starts in PREVIEW; nothing is shared until its operators say LIVE).")
                }
            }
            LeftNotice { chan, net } => {
                if it {
                    format!("{chan} su {net} ha lasciato questo link.")
                } else {
                    format!("{chan} on {net} left this link.")
                }
            }
            Unlinked { chan } => {
                if it {
                    format!("{chan} non è più collegato. Niente di tuo viene più condiviso.")
                } else {
                    format!("{chan} is no longer linked. Nothing of yours is shared any more.")
                }
            }
            NotLinked { chan } => {
                if it {
                    format!("{chan} non è collegato.")
                } else {
                    format!("{chan} is not linked.")
                }
            }
            Paused { chan } => {
                if it {
                    format!("{chan}: link in pausa. Non passa niente finché non fai RESUME.")
                } else {
                    format!("{chan}: link paused. Nothing flows until RESUME.")
                }
            }
            Resumed { chan } => {
                if it {
                    format!("{chan}: link ripreso.")
                } else {
                    format!("{chan}: link resumed.")
                }
            }
            ModeLive { chan } => {
                if it {
                    format!("{chan} ora è LIVE. La chat passa da e verso ogni canale collegato che è LIVE a sua volta.")
                } else {
                    format!("{chan} is now LIVE. Chat flows to and from every linked channel that is LIVE too.")
                }
            }
            ModePreview { chan } => {
                if it {
                    format!("{chan} è in PREVIEW: non si invia e non si mostra nulla. SAMPLE mostra cosa avresti visto.")
                } else {
                    format!("{chan} is in PREVIEW: nothing is sent or shown. SAMPLE shows what you would have seen.")
                }
            }
            ModeOff { chan } => {
                if it {
                    format!("{chan} è OFF: collegato solo sulla carta.")
                } else {
                    format!("{chan} is OFF: linked on paper only.")
                }
            }
            SetOk { chan, key, value } => format!("{chan}: {key} = {value}"),
            Blocked { chan, mask } => {
                if it {
                    format!("{chan}: {mask} è bloccato.")
                } else {
                    format!("{chan}: {mask} is blocked.")
                }
            }
            Unblocked { chan, mask } => {
                if it {
                    format!("{chan}: {mask} non è più bloccato.")
                } else {
                    format!("{chan}: {mask} is no longer blocked.")
                }
            }
            NotBlocked { mask } => {
                if it {
                    format!("{mask} non è nella lista dei blocchi.")
                } else {
                    format!("{mask} is not on the block list.")
                }
            }
            BlockFull => {
                if it {
                    "La lista dei blocchi è piena (200 voci).".into()
                } else {
                    "The block list is full (200 entries).".into()
                }
            }
            BadMask => {
                if it {
                    "La maschera deve avere da 1 a 100 caratteri.".into()
                } else {
                    "The mask must be 1-100 characters.".into()
                }
            }
            Muted { chan, other } => {
                if it {
                    format!("{chan}: non senti più {other}. Loro ti sentono ancora, a meno che non ti silenzino a loro volta.")
                } else {
                    format!("{chan}: you no longer hear {other}. They still hear you unless they mute you too.")
                }
            }
            Unmuted { chan, other } => {
                if it {
                    format!("{chan}: {other} di nuovo udibile.")
                } else {
                    format!("{chan}: {other} unmuted.")
                }
            }
            SampleNone => {
                if it {
                    "Ancora niente da mostrare: nessun canale collegato in LIVE ha detto qualcosa."
                        .into()
                } else {
                    "Nothing to show yet: no linked channel that is LIVE has said anything.".into()
                }
            }
            SampleHeader { n, chan } => {
                if it {
                    format!(
                        "Ultime {n} righe che {chan} avrebbe mostrato (non è stato inviato nulla):"
                    )
                } else {
                    format!("Last {n} lines {chan} would have shown (nothing was actually sent):")
                }
            }
            AuditNone => {
                if it {
                    "Ancora nessuna voce di registro per questo canale.".into()
                } else {
                    "No audit entries for this channel yet.".into()
                }
            }
            NetDown { label } => {
                if it {
                    format!("{label} è scollegata dal link in questo momento; la sua gente tornerà quando si riconnette.")
                } else {
                    format!("{label} is disconnected from the link right now; its people will be back when it returns.")
                }
            }
        }
    }
}

/// `HELP` and `HELP SET` text.
pub fn help_lines(topic: Option<&str>, svc: &str, lang: Lang) -> Vec<String> {
    let it = lang == Lang::It;
    match topic {
        Some("SET") => {
            let mut v = vec![if it {
                "SET <#canale> <impostazione> <valore> - il tuo canale, le tue regole:".to_string()
            } else {
                "SET <#channel> <setting> <value> - your channel, your rules:".to_string()
            }];
            v.extend(crate::policy::Policy::keys(lang).iter().map(|(k, h)| format!("  {k}: {h}")));
            v
        }
        _ if it => vec![
            format!("{svc}: collega il tuo canale con canali di altre reti. Il controllo resta a te."),
            "Prima fai /INVITE a me nel tuo canale. Solo gli operatori del canale possono usare questi comandi.".into(),
            "  LINK <#tuo> <rete> <#loro>       chiedi un link (loro devono fare ACCEPT; entrambi partono in PREVIEW)".into(),
            "  ACCEPT / DENY <#tuo> <rete> <#loro>".into(),
            "  PREVIEW | LIVE | OFF <#tuo>      non passa nulla finché ENTRAMBI i lati sono LIVE".into(),
            "  SAMPLE <#tuo>                    vedi cosa avrebbe mostrato la preview".into(),
            "  SET <#tuo> <impostazione> <valore>   HELP SET le elenca".into(),
            "  BLOCK / UNBLOCK <#tuo> <maschera>    maschera: nick!ident@host oppure account:nome".into(),
            "  MUTE / UNMUTE <#tuo> <rete> <#loro>".into(),
            "  PAUSE / RESUME / UNLINK <#tuo>   stop immediato, quando vuoi".into(),
            "  STATUS / AUDIT <#tuo>            cosa succede e chi ha cambiato cosa".into(),
        ],
        _ => vec![
            format!("{svc}: link your channel with channels on other networks. You stay in control."),
            "First /INVITE me into your channel. Only channel operators can use these commands.".into(),
            "  LINK <#you> <network> <#them>    ask to link (they must ACCEPT; both start in PREVIEW)".into(),
            "  ACCEPT / DENY <#you> <network> <#them>".into(),
            "  PREVIEW | LIVE | OFF <#you>      nothing flows until BOTH sides are LIVE".into(),
            "  SAMPLE <#you>                    see what preview would have shown".into(),
            "  SET <#you> <setting> <value>     HELP SET lists them".into(),
            "  BLOCK / UNBLOCK <#you> <mask>    mask: nick!ident@host or account:name".into(),
            "  MUTE / UNMUTE <#you> <network> <#them>".into(),
            "  PAUSE / RESUME / UNLINK <#you>   instant stop, any time".into(),
            "  STATUS / AUDIT <#you>            what is going on, who changed what".into(),
        ],
    }
}
