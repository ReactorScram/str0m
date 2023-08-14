mod common;
use common::init_log;
use str0m::format::Codec;
use str0m::format::CodecSpec;
use str0m::format::FormatParams;
use str0m::format::PayloadParams;
use str0m::media::Direction;
use str0m::media::MediaKind;
use str0m::rtp::{Extension, ExtensionMap};
use str0m::Rtc;
use tracing::info_span;

#[test]
pub fn change_default_pt() {
    init_log();

    // First proposed PT is 100, R side adjusts its default from 102 -> 100
    let (l, r) = with_params(
        //
        &[opus(100)],
        &[opus(102)],
    );

    // Test left side.
    assert_eq!(&[opus(100)], &**l.codec_config());
    assert!(l.codec_config()[0].is_locked());

    // Test right side.
    assert_eq!(&[opus(100)], &**r.codec_config());
    assert!(r.codec_config()[0].is_locked());
}

#[test]
pub fn answer_change_order() {
    init_log();

    // First proposed PT are 100/102, but R side has a different order.
    let (l, r) = with_params(
        //
        &[vp8(100), h264(102)],
        &[h264(96), vp8(98)],
    );

    let mid = l.media_mids()[0];

    // Test left side.
    assert_eq!(&[vp8(100), h264(102)], &**l.codec_config());
    assert!(l.codec_config().iter().all(|p| p.is_locked()));
    assert_eq!(
        l.media(mid).unwrap().remote_pts(),
        // R side has expressed its preference order, but amended the PT to match the OFFER.
        &[102.into(), 100.into()]
    );

    // Test right side.
    assert_eq!(&[h264(102), vp8(100)], &**r.codec_config());
    assert!(r.codec_config().iter().all(|p| p.is_locked()));
    assert_eq!(
        r.media(mid).unwrap().remote_pts(),
        // OFFER straight up.
        &[100.into(), 102.into()]
    );
}

#[test]
pub fn answer_narrow() {
    init_log();

    // First proposed PT are 100/102, the R side removes unsupported ones.
    let (l, r) = with_params(
        //
        &[vp8(100), h264(102)],
        &[h264(96)],
    );

    let mid = l.media_mids()[0];

    // Test left side.
    assert_eq!(&[vp8(100), h264(102)], &**l.codec_config());
    assert_eq!(
        l.codec_config()
            .iter()
            .map(|p| p.is_locked())
            .collect::<Vec<_>>(),
        // VP8 is not locked, H264 is
        vec![false, true]
    );
    assert_eq!(
        l.media(mid).unwrap().remote_pts(),
        // R side has narrowed the remote_pts to only the supported.
        &[102.into()]
    );

    // Test right side. The PT of h264 is updated with what L OFFERed.
    assert_eq!(&[h264(102)], &**r.codec_config());
    assert!(r.codec_config().iter().all(|p| p.is_locked()));
    assert_eq!(
        r.media(mid).unwrap().remote_pts(),
        // OFFER straight up.
        &[102.into()]
    );
}

#[test]
pub fn answer_no_match() {
    init_log();

    // L has one codec, and that is not matched by R. This should disable the m-line.
    let (l, r) = with_params(
        //
        &[vp8(100)],
        &[h264(96)],
    );

    let mid = l.media_mids()[0];

    // Test left side. Nothing has changed. The codec is not locked.
    assert_eq!(&[vp8(100)], &**l.codec_config());
    assert!(!l.codec_config()[0].is_locked());
    // No remote PTs.
    assert_eq!(l.media(mid).unwrap().remote_pts(), &[]);

    // Test right side. Nothing has changed. The codec is not locked.
    assert_eq!(&[h264(96)], &**r.codec_config());
    assert!(!r.codec_config()[0].is_locked());
    // No remote PTs.
    assert_eq!(r.media(mid).unwrap().remote_pts(), &[]);

    // TODO: here we should check for the m-line being made inactive by setting the port to 0.
}

#[test]
fn narrow_exts() {
    init_log();

    use Extension::*;

    let exts_l = ExtensionMap::standard();
    let mut exts_r = ExtensionMap::empty();

    // Not same number as the default.
    exts_r.set(14, TransportSequenceNumber);
    exts_r.set(12, AudioLevel);

    // This negotiates a video track.
    let (l, r) = with_exts(exts_l, exts_r);

    let v_l: Vec<_> = l.exts().iter(false).collect();
    let v_r: Vec<_> = r.exts().iter(false).collect();
    let a_l: Vec<_> = l.exts().iter(true).collect();
    let a_r: Vec<_> = r.exts().iter(true).collect();

    // L only keeps 3 (the default) for TransportSequenceNumber.
    // R only keeps 3, and changes it from 15.
    assert_eq!(v_l, vec![(3, TransportSequenceNumber)]);
    assert_eq!(v_r, vec![(3, TransportSequenceNumber)]);

    // L audio exts are left untouched (the defaults), also ones shared with video.
    // R audio exts are left untouched.
    assert_eq!(
        a_l,
        vec![
            (1, AudioLevel),
            (2, AbsoluteSendTime),
            (3, TransportSequenceNumber),
            (4, RtpMid),
            (10, RtpStreamId),
            (11, RepairedRtpStreamId)
        ]
    );
    assert_eq!(a_r, vec![(3, TransportSequenceNumber), (12, AudioLevel)]);
}

fn with_params(params_l: &[PayloadParams], params_r: &[PayloadParams]) -> (Rtc, Rtc) {
    let mut l = build_params(params_l);
    let mut r = build_params(params_r);

    let kind = params_l
        .first()
        .map(|p| p.spec().codec.kind())
        .unwrap_or(MediaKind::Audio);

    negotiate(&mut l, &mut r, kind);

    (l, r)
}

fn with_exts(exts_l: ExtensionMap, exts_r: ExtensionMap) -> (Rtc, Rtc) {
    let mut l = build_exts(exts_l);
    let mut r = build_exts(exts_r);

    negotiate(&mut l, &mut r, MediaKind::Video);

    (l, r)
}

fn negotiate(l: &mut Rtc, r: &mut Rtc, kind: MediaKind) {
    let dir = Direction::SendRecv;

    let (offer, pending) = {
        let span = info_span!("L");
        let _e = span.enter();
        let mut change = l.sdp_api();
        change.add_media(kind, dir, None, None);

        change.apply().unwrap()
    };
    println!("L {:#?}", offer);
    let answer = {
        let span = info_span!("R");
        let _e = span.enter();
        r.sdp_api().accept_offer(offer).unwrap()
    };
    println!("R {:#?}", answer);
    {
        let span = info_span!("L");
        let _e = span.enter();
        l.sdp_api().accept_answer(pending, answer).unwrap();
    }
}

fn build_params(params: &[PayloadParams]) -> Rtc {
    let mut b = Rtc::builder().clear_codecs();
    let config = b.codec_config();
    for p in params {
        config.add_config(
            p.pt(),
            p.resend(),
            p.spec().codec,
            p.spec().clock_rate,
            p.spec().channels,
            p.spec().format,
        );
    }
    b.build()
}

fn build_exts(exts: ExtensionMap) -> Rtc {
    let mut b = Rtc::builder().clear_codecs();
    let e = b.extension_map();
    *e = exts;
    b.build()
}

fn opus(pt: u8) -> PayloadParams {
    PayloadParams::new(
        pt.into(),
        None,
        CodecSpec {
            codec: Codec::Opus,
            channels: Some(2),
            clock_rate: 48_000,
            format: FormatParams::default(),
        },
    )
}

fn vp8(pt: u8) -> PayloadParams {
    PayloadParams::new(
        pt.into(),
        None,
        CodecSpec {
            codec: Codec::Vp8,
            channels: None,
            clock_rate: 90_000,
            format: FormatParams::default(),
        },
    )
}

fn h264(pt: u8) -> PayloadParams {
    PayloadParams::new(
        pt.into(),
        None,
        CodecSpec {
            codec: Codec::H264,
            channels: None,
            clock_rate: 90_000,
            format: FormatParams::default(),
        },
    )
}