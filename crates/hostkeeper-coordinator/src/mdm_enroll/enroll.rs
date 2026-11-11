//! `POST /enroll`: the device blob in, the enrollment profile out (see the module above for the
//! rules). The ABM table is read here and again at an operator's confirmation and at
//! `Authenticate`: the model check of enrollment found that a serial that left ABM between two of
//! them must not be let through by the earlier read.

use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use hostkeeper_mdm_ade::blob::{self, Device};
use hostkeeper_mdm_ade::profile::{self, Enrollment};
use hyper::Method;
use sha2::{Digest, Sha256};

use super::pending::Held1;
use super::{Answer, Decline, Tunables, ca, ca_unavailable, internal, reply};
use crate::audit_actions;
use crate::mdm_server::MdmServer;
use crate::service::Coordinator;
use crate::store::mdm::Challenge;

/// The identifier of the enrollment profile the Macs are given.
pub(crate) const PROFILE_IDENTIFIER: &str = "dev.hostkeeper.mdm";

/// A serial as the tables and the identity's subject carry it: letters and digits, 1 to 64.
pub(crate) fn is_serial(text: &str) -> bool {
    (1..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Answers a request to `/enroll`.
pub(crate) async fn answer(
    coordinator: &Coordinator,
    server: &MdmServer,
    peer: Option<IpAddr>,
    method: &Method,
    body: &[u8],
) -> Answer {
    if *method == Method::GET {
        // No device speaks in a GET, so no serial is proven to be ours: a manual enrollment is
        // declined as one the ABM table does not list.
        return Decline::NotInAbm.answer();
    }
    if *method != Method::POST {
        return Decline::Method.answer();
    }
    match post(coordinator, server, peer, body).await {
        Ok(profile) => reply("application/x-apple-aspen-config", profile),
        Err(decline) => decline.answer(),
    }
}

fn audit(coordinator: &Coordinator, action: &str, serial: &str, reason: String) {
    // A row that cannot be written changes nothing the Mac is told: the request is held or
    // declined either way.
    let _ = coordinator.audit_setup("coordinator", action, Some(serial), reason);
}

/// What the blob said and whether it was verified.
fn read_blob(
    server: &MdmServer,
    body: &[u8],
    now: i64,
) -> Result<(Device, Option<String>), Decline> {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap_or(0));
    match blob::verify(body, &server.enroll.roots, at) {
        Ok(device) => Ok((device, None)),
        Err(why) => blob::claims(body)
            .map(|device| (device, Some(why.to_string())))
            .map_err(|_| Decline::BadRequest),
    }
}

async fn post(
    coordinator: &Coordinator,
    server: &MdmServer,
    peer: Option<IpAddr>,
    body: &[u8],
) -> Result<Vec<u8>, Decline> {
    let tune = Tunables::read(&coordinator.mdm_setup).ok_or(Decline::Unavailable)?;
    let now = server.now();
    if !server.enroll.limiter.allow(peer, now, tune.rate_per_min) {
        return Err(Decline::RateLimited);
    }
    let (Some(dns_name), Ok(Some(push))) = (
        tune.dns_name.as_deref(),
        coordinator.mdm_setup.push_identity(),
    ) else {
        return Err(Decline::Unavailable);
    };
    let (device, unverified) = read_blob(server, body, now)?;
    if !is_serial(&device.serial) {
        return Err(Decline::BadRequest);
    }
    if !server.serial_in_abm(&device.serial).map_err(internal)? {
        let (window, max) = (i64::from(tune.challenge_ttl_s), tune.pending_max as usize);
        if server
            .enroll
            .pending
            .first_decline(&device.serial, now, window, max)
        {
            audit(
                coordinator,
                audit_actions::MDM_ENROLL_DECLINED,
                &device.serial,
                "the serial is not in the ABM table".to_owned(),
            );
        }
        return Err(Decline::NotInAbm);
    }
    let confirmed = unverified.is_some();
    if let Some(reason) = unverified {
        if !server
            .enroll
            .pending
            .spend(&device.serial, &device.udid, now)
        {
            hold(coordinator, server, &tune, &device, &reason, now)?;
            return Err(Decline::Held);
        }
    }
    // The CA is made here if this is the first need, so that the Mac's SCEP request finds it.
    ca::identity_ca(coordinator, server, &tune)
        .await
        .map_err(ca_unavailable)?;
    let challenge = new_challenge(server)?;
    server
        .store
        .issue_challenge(&Challenge {
            challenge_sha256: hex::encode(Sha256::digest(challenge.as_bytes())),
            serial: device.serial.clone(),
            udid: device.udid.clone(),
            expires_unix: now.saturating_add(i64::from(tune.challenge_ttl_s)),
            confirmed,
        })
        .map_err(internal)?;
    profile::render(&Enrollment {
        dns_name,
        identifier: PROFILE_IDENTIFIER,
        display_name: &tune.profile_name,
        organization: &tune.org,
        topic: &push.info.topic,
        challenge: &challenge,
    })
    .map_err(internal)
}

/// Holds the request of an unverified blob for an operator, and audits the first post of it.
fn hold(
    coordinator: &Coordinator,
    server: &MdmServer,
    tune: &Tunables,
    device: &Device,
    reason: &str,
    now: i64,
) -> Result<(), Decline> {
    let held = server.enroll.pending.hold(
        &device.serial,
        &device.udid,
        device.product.as_deref(),
        reason,
        now,
        tune.pending_max as usize,
    );
    match held {
        Held1::Full => Err(Decline::Unavailable),
        Held1::Seen => Ok(()),
        Held1::New => {
            audit(
                coordinator,
                audit_actions::MDM_ENROLL_HELD,
                &device.serial,
                format!("the blob is not verified ({reason}): the request waits for an operator"),
            );
            Ok(())
        }
    }
}

/// A new challenge: 32 random bytes in hex.
fn new_challenge(server: &MdmServer) -> Result<String, Decline> {
    let mut bytes = [0_u8; 32];
    if !server.random(&mut bytes) {
        tracing::error!("the system gave no random bytes for a challenge");
        return Err(Decline::Internal);
    }
    Ok(hex::encode(bytes))
}
