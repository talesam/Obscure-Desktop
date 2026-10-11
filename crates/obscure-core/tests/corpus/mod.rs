//! Share links covering every protocol, transport and security combination
//! the parser understands. The values are well-formed enough for
//! `xray run -test` (real key lengths, hex short ids…).

#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

pub const UUID: &str = "2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c";
/// x25519 public key (32 bytes, base64url).
pub const PBK: &str = "RApWyxh62PUorKsccEnjo-KVi3OWKOtsyEhq5xgk9CU";
/// WireGuard keys (32 bytes, standard base64).
pub const WG_PRIV: &str = "YNXtAzepDqRv9H52osJVDQnznT5AL11eVb2aNhnWAA0=";
pub const WG_PUB: &str = "Z1XXLsKYkYxuiYjJIkRvtIKFepCYHTgON+GwPq7SOV4=";
pub const CERT_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// ML-DSA-65 public key: 1952 bytes, base64url.
pub fn pqv() -> String {
    let bytes: Vec<u8> = (0..1952u32).map(|i| (i * 7 % 251) as u8).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn vmess_json(json: &str) -> String {
    format!("vmess://{}", STANDARD.encode(json))
}

pub fn links() -> Vec<String> {
    let pqv = pqv();
    let extra = "%7B%22xPaddingBytes%22%3A%22100-1000%22%2C%22noGRPCHeader%22%3Afalse%7D";
    let ss_legacy = STANDARD.encode("chacha20-ietf-poly1305:pw@legacy.example:9000");
    let ss_b64 = URL_SAFE_NO_PAD.encode("aes-256-gcm:secret");
    let socks_b64 = STANDARD.encode("u:p");
    vec![
        // VLESS
        format!(
            "vless://{UUID}@example.org:443?encryption=none&flow=xtls-rprx-vision&security=reality&sni=www.yahoo.com&fp=random&pbk={PBK}&sid=95&spx=%2Fabc&type=tcp#Reality%20Vision"
        ),
        format!(
            "vless://{UUID}@203.0.113.7:443?security=reality&sni=www.microsoft.com&fp=chrome&pbk={PBK}&sid=6ba85179e30d4fc2&pqv={pqv}&type=tcp&flow=xtls-rprx-vision#PQ"
        ),
        format!(
            "vless://{UUID}@[2001:db8::1]:8443?type=ws&path=%2Fchat%3Fed%3D2048&host=cdn.example.com&security=tls&sni=cdn.example.com&alpn=h2,http/1.1&fp=chrome#WS%20IPv6"
        ),
        format!(
            "vless://{UUID}@104.16.1.1:443?type=ws&path=%2Fws&host=cdn.example.com&security=tls#WS%20CDN%20IP"
        ),
        format!(
            "vless://{UUID}@g.example:443?type=grpc&serviceName=svc&mode=multi&security=tls&sni=g.example#gRPC%20multi"
        ),
        format!(
            "vless://{UUID}@g.example:80?type=grpc&serviceName=svc&mode=gun&security=none#gRPC%20gun"
        ),
        format!(
            "vless://{UUID}@x.example:443?type=xhttp&path=%2Fx&host=x.example&mode=stream-one&extra={extra}&security=reality&sni=x.example&pbk={PBK}&sid=ab#XHTTP%20extra"
        ),
        format!(
            "vless://{UUID}@198.51.100.2:443?type=splithttp&path=%2Fsplit&host=front.example&mode=packet-up&security=tls&alpn=h3#XHTTP%20IP"
        ),
        format!(
            "vless://{UUID}@u.example:443?type=httpupgrade&path=%2Fup&host=u.example&security=tls&fp=firefox#HTTPUpgrade"
        ),
        format!(
            "vless://{UUID}@k.example:4000?type=kcp&seed=s33d&headerType=wechat-video&security=none#mKCP"
        ),
        format!("vless://{UUID}@t.example:80?type=tcp&headerType=http&security=none#TCP%20http"),
        format!(
            "vless://{UUID}@p.example:443?security=tls&sni=p.example&pcs={CERT_SHA256}&vcn=real.example#Pinned"
        ),
        format!(
            "vless://{UUID}@i.example:443?security=tls&sni=i.example&allowInsecure=1#Insecure"
        ),
        format!("vless://{UUID}@raw.example:443?type=raw&security=none"),
        // VMess (JSON, with the type quirks of real generators)
        vmess_json(&format!(
            r#"{{"v":"2","ps":"VMess WS","add":"vm.example.com","port":"443","id":"{UUID}","aid":"0","scy":"auto","net":"ws","type":"none","host":"vm.example.com","path":"/ws","tls":"tls","sni":"vm.example.com","alpn":"","fp":"chrome"}}"#
        )),
        vmess_json(&format!(
            r#"{{"v":2,"ps":"VMess nulls","add":"vm2.example","port":8080,"id":"{UUID}","aid":0,"scy":null,"net":"tcp","type":null,"host":null,"path":null,"tls":null,"sni":null,"alpn":null,"fp":null}}"#
        )),
        vmess_json(&format!(
            r#"{{"ps":"VMess gRPC","add":"vg.example","port":"443","id":"{UUID}","net":"grpc","type":"multi","path":"grpcsvc","tls":"tls","alpn":["h2"]}}"#
        )),
        vmess_json(&format!(
            r#"{{"ps":"VMess XHTTP","add":"104.16.2.2","port":443,"id":"{UUID}","net":"xhttp","type":"auto","host":"vx.example","path":"/xh","tls":"tls"}}"#
        )),
        vmess_json(&format!(
            r#"{{"ps":"VMess kcp","add":"vk.example","port":"4001","id":"{UUID}","net":"kcp","type":"srtp","path":"seed","scy":"aes-128-gcm"}}"#
        )),
        vmess_json(&format!(
            r#"{{"ps":"VMess upgrade","add":"vu.example","port":"80","id":"{UUID}","net":"httpupgrade","host":"vu.example","path":"/u","scy":"chacha20-poly1305"}}"#
        )),
        vmess_json(&format!(
            r#"{{"ps":"VMess insecure","add":"vi.example","port":"443","id":"{UUID}","net":"tcp","tls":"tls","allowInsecure":true}}"#
        )),
        format!(
            "vmess://{UUID}@vr.example:443?encryption=auto&type=tcp&security=reality&sni=www.yahoo.com&pbk={PBK}&sid=95#VMess%20Reality"
        ),
        format!(
            "vmess://{UUID}@vp.example:443?encryption=zero&type=grpc&serviceName=s&mode=multi&security=tls&pcs={CERT_SHA256}#VMess%20pinned"
        ),
        // Trojan
        "trojan://p%40ss@tj.example.com:443?sni=tj.example.com#Trojan".to_owned(),
        "trojan://pw@tw.example:443?type=ws&path=%2Ftr&host=tw.example&security=tls&fp=safari#Trojan%20WS"
            .to_owned(),
        "trojan://pw@tg.example:443?type=grpc&serviceName=tg&security=tls#Trojan%20gRPC".to_owned(),
        format!(
            "trojan://pw@tr.example:443?security=reality&sni=www.apple.com&pbk={PBK}&sid=01#Trojan%20Reality"
        ),
        "trojan://pw@tn.example:8080?security=none#Trojan%20plain".to_owned(),
        // Shadowsocks
        format!("ss://{ss_b64}@ss.example.com:8388#SS%20Node"),
        "ss://2022-blake3-aes-128-gcm:AAAAAAAAAAAAAAAAAAAAAA%3D%3D@s1.example:443#SS2022-128".to_owned(),
        "ss://2022-blake3-aes-256-gcm:YctPZ6U7xPPcU%2Bgp3u%2B0tx%2FtRizJN9K8y%2BuKlW2qjlI%3D@[2001:db8::5]:443#SS2022-256"
            .to_owned(),
        format!("ss://{ss_legacy}#Legacy"),
        // WireGuard
        format!(
            "wireguard://{}@wg.example:51820?publickey={}&address=172.16.0.2/32,fd00::2/128&reserved=1,2,3&mtu=1280#WG",
            urlencode(WG_PRIV),
            urlencode(WG_PUB)
        ),
        format!(
            "wg://{}@162.159.192.1:2408?publickey={}&presharedkey={}#WARP",
            urlencode(WG_PRIV),
            urlencode(WG_PUB),
            urlencode(WG_PUB)
        ),
        // SOCKS
        "socks://usr:p%40ss@s.example:1080#SOCKS".to_owned(),
        "socks5://s.example:1080".to_owned(),
        format!("socks://{socks_b64}@s.example:1080#SOCKS%20b64"),
        "socks://bob@s.example:1080#User%20only".to_owned(),
        "socks://u:p@st.example:1443?security=tls&sni=st.example#SOCKS%20TLS".to_owned(),
    ]
}

fn urlencode(s: &str) -> String {
    s.replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D")
}
