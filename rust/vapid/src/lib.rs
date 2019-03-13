//! VAPID auth support
//!
//! This library only supports the latest VAPID-draft-02+ specification.
//!
//! Example Use:
//! ```rust,no_run
//! use vapid::{Key, sign};
//! use std::collections::HashMap;
//!
//! // Create a key from an existing EC Private Key PEM file.
//! // You can generate this with
//! // Key::generate().to_pem("pem/file/path.pem");
//! let my_key = Key::from_pem("pem/file/path.pem").unwrap();
//!
//! // Construct the Claims hashmap
//! let mut claims:HashMap<String, serde_json::Value> = HashMap::new();
//! claims.insert(
//!     String::from("sub"), serde_json::Value::from("mailto:bob@example.com")
//! );
//! // while `exp` can be filled in for you, `aud` should point to the net location of the
//! // Push server you wish to talk to. (e.g. `https://push.services.mozilla.org`)
//! // `aud` is optional for Mozilla, but may be required for GCM/FCM or other systems.
//! claims.insert(
//!     String::from("aud"), serde_json::Value::from("https://host.ext")
//! );
//!
//! // The result will contain the `Authorization:` header. How you inject this into your
//! // request is left as an exercise.
//! let authorization_header = sign(my_key, &mut claims).unwrap();
//!
//! ```

extern crate base64;
extern crate openssl;
extern crate serde_json;
extern crate time;

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;

use openssl::bn::BigNumContext;
use openssl::ec;
use openssl::hash::MessageDigest;
use openssl::nid;
use openssl::pkey;
use openssl::sign::{Signer, Verifier};

pub struct Key(ec::EcKey);

/// a Key is a helper for creating or using a VAPID EC key.
///
/// Vapid Keys are always Prime256v1 EC keys.
///
impl Key {
    fn name() -> nid::Nid {
        nid::X9_62_PRIME256V1
    }

    fn group() -> ec::EcGroup {
        ec::EcGroup::from_curve_name(Key::name()).expect("EC Prime256v1 curve not supported")
    }

    /// Read a VAPID private key stored in `path`
    pub fn from_pem<P>(path: P) -> Result<Key, String>
    where
        P: AsRef<Path>,
    {
        let mut pem_data = Vec::new();
        File::open(&path)
            .expect(&format!("Could not open file at {:?}", &path.as_ref()))
            .read_to_end(&mut pem_data)
            .expect(&format!("Could not read from file {:?}", &path.as_ref()));
        let key_data =
            ec::EcKey::private_key_from_pem(&pem_data).expect("Could not read PEM key data");
        Ok(Key(key_data))
    }

    /// Write the VAPID private key as a PEM to `path`
    pub fn to_pem(&self, path: &Path) -> Result<(), String> {
        let key_data = self
            .0
            .private_key_to_pem()
            .expect("Could not generate PEM for privakte key");
        File::create(&path)
            .expect(&format!(
                "Could not open file at {:?}",
                path.to_str().unwrap()
            ))
            .write(&key_data)
            .expect(&format!(
                "Could not write PEM data to file {}",
                path.to_str().unwrap()
            ));
        Ok(())
    }

    /// Create a new Vapid key
    pub fn generate() -> Key {
        Key(ec::EcKey::generate(&Key::group()).expect("Could not generate EC key"))
    }

    /// Convert the private key into a base64 string
    pub fn to_private_raw(&self) -> String {
        // Return the private key as a raw bit array
        let key = self.0.private_key().expect("Could not extract private key");
        base64::encode_config(&key.to_vec(), base64::URL_SAFE_NO_PAD)
    }

    /// Convert the public key into a uncompressed, raw base64 string
    pub fn to_public_raw(&self) -> String {
        //Return the public key as a raw bit array
        let mut ctx = BigNumContext::new().unwrap();
        let group = Key::group();

        let key = self.0.public_key().unwrap();
        let keybytes = key
            .to_bytes(&group, ec::POINT_CONVERSION_UNCOMPRESSED, &mut ctx)
            .unwrap();
        base64::encode_config(&keybytes, base64::URL_SAFE_NO_PAD)
    }

    /// Read the public key from an uncompressed, raw base64 string
    pub fn from_public_raw(bits: String) -> Result<Key, String> {
        //Read a public key from a raw bit array
        let bytes: Vec<u8> =
            base64::decode_config(&bits.into_bytes(), base64::URL_SAFE_NO_PAD).unwrap();
        let group = Key::group();
        if bytes.len() != 65 || bytes[0] != 4 {
            // It's not a properly tagged key.
            return Err("Invalid key format for public key.".into());
        }

        let x = openssl::bn::BigNum::from_slice(&bytes[1..33]).expect("Invalid public key");
        let y = openssl::bn::BigNum::from_slice(&bytes[33..]).expect("Invalid public key");
        let mut new_key = match ec::EcKeyBuilder::new() {
            Ok(key) => key,
            Err(err) => return Err(format!("Could not generate key: {}", err)),
        };
        new_key.set_group(&group).expect("Could not set group");
        new_key
            .set_public_key_affine_coordinates(&x, &y)
            .expect("Invalid coordiates for public key");
        Ok(Key(new_key.build()))
    }
}

#[derive(Debug)]
struct AuthElements {
    t: Vec<String>,
    k: String,
}

/// Parse the Authorization Header for useful things.
fn parse_auth_token(auth_token: &str) -> Result<AuthElements, String> {
    let mut parts: Vec<&str> = auth_token.split(" ").collect();
    let mut schema = parts.remove(0).to_lowercase();
    // Ignore the first token if it's the header line.
    if schema == "authorization:" {
        schema = parts.remove(0).to_lowercase();
    }
    let mut reply: AuthElements = AuthElements {
        t: Vec::new(),
        k: String::new(),
    };
    match schema.to_lowercase().as_ref() {
        "vapid" => {
            for kvi in parts[0].splitn(2, ",") {
                let kv: Vec<String> = kvi.splitn(2, "=").map(|x| String::from(x)).collect();
                match kv[0].to_lowercase().as_ref() {
                    "t" => {
                        let ts: Vec<String> = kv[1].split(".").map(|x| String::from(x)).collect();
                        if ts.len() != 3 {
                            return Err("Invalid t token specified".into());
                        }
                        let ttoken = format!("{}.{}", ts[0], ts[1]);
                        reply.t = vec![ttoken, ts[2].clone()];
                    }
                    "k" => reply.k = String::from(kv[1].clone()),
                    _ => {}
                }
            }
        }
        "webpush" => {
            reply.t = parts[0].split(".").map(|x| String::from(x)).collect();
        }
        _ => return Err(format!("Unknown schema type: {}", parts[0])),
    };
    return Ok(reply);
}

// Preferred schema
static SCHEMA: &str = "vapid";

/// Convert the HashMap containing the claims into an Authorization header.
/// `key` must be generated or initialized before this is used. See `Key::from_pem()` or
/// `Key::generate()`.
pub fn sign(key: Key, claims: &mut HashMap<String, serde_json::Value>) -> Result<String, String> {
    // this is the common, static header for all VAPID JWT objects.
    let prefix: String = "{\"typ\":\"JWT\",\"alg\":\"ES256\"}".into();

    // Check the claims
    match claims.get("sub") {
        Some(sub) => {
            if !sub.as_str().unwrap().starts_with("mailto") {
                return Err("\"sub\" not a valid HTML reference".into());
            }
        }
        None => return Err("\"sub\" not found".into()),
    }
    let today = time::now_utc();
    let tomorrow = today + time::Duration::hours(24);
    claims
        .entry(String::from("exp"))
        .or_insert(serde_json::Value::from(tomorrow.to_timespec().sec));
    match claims.get("exp") {
        Some(exp) => {
            let exp_val = exp.as_i64().unwrap();
            if exp_val < today.to_timespec().sec {
                return Err(String::from("\"exp\" already expired"));
            }
            if exp_val > tomorrow.to_timespec().sec {
                return Err(String::from("\"exp\" set too far ahead"));
            }
        }
        None => {
            // We already do an insertion on empty, so this should never trigger.
            return Err(String::from("\"exp\" failed to initialize"));
        }
    }

    let json: String = serde_json::to_string(&claims)
        .expect("Claims cannot be converted to a valid JSON structure.");
    let content = format!(
        "{}.{}",
        base64::encode_config(&prefix, base64::URL_SAFE_NO_PAD),
        base64::encode_config(&json, base64::URL_SAFE_NO_PAD)
    );
    let auth_k = key.to_public_raw();
    let pub_key = &pkey::PKey::from_ec_key(key.0).unwrap();

    let mut signer = match Signer::new(MessageDigest::sha256(), pub_key) {
        Ok(t) => t,
        Err(err) => return Err(format!("Could not sign the claims: {:?}", err)),
    };
    signer
        .update(&content.clone().into_bytes())
        .expect("Could not encode data for signature");
    let signature = signer.sign_to_vec().expect("Could not finalize signature");

    // Decode signature BER to r,s pair
    let r_off: usize = 3;
    // r_len must be > 33. Not checking here because if this ever breaks, we have LOTS of other
    // problems.
    let r_len = signature[r_off] as usize;
    // calculate the offsets for the byte array data we want.
    let s_off: usize = r_off + r_len + 2;
    let s_len = signature[s_off] as usize;
    let mut r_val = &signature[(r_off + 1)..(r_off + 1 + r_len)];
    let mut s_val = &signature[(s_off + 1)..(s_off + 1 + s_len)];
    // Strip the leading 0 if it's present.
    if r_len == 33 && r_val[0] == 0 {
        r_val = &r_val[1..];
    }
    if s_len == 33 && s_val[0] == 0 {
        s_val = &s_val[1..];
    }
    // we now have the r and s byte arrays. Build the raw RS we need for the signature
    // println!("r_val: ({}){:?}\ns_val: ({}){:?} ", r_val.len(), r_val, s_val.len(), s_val);
    let mut sigval: Vec<u8> = Vec::with_capacity(64);
    sigval.extend(r_val);
    sigval.extend(s_val);

    let auth_t = format!(
        "{}.{}",
        content.clone(),
        base64::encode_config(
            unsafe { &String::from_utf8_unchecked(sigval) },
            base64::URL_SAFE_NO_PAD,
        )
    );

    Ok(format!(
        "Authorization: {} t={},k={}",
        SCHEMA, auth_t, auth_k
    ))
}

pub fn verify(auth_token: String) -> Result<HashMap<String, serde_json::Value>, String> {
    //Verify that the auth token string matches for the verification token string
    let auth_token = parse_auth_token(&mut String::from(auth_token.clone()))
        .expect("Authorization header is invalid.");
    let pub_ec_key =
        Key::from_public_raw(auth_token.k).expect("'k' token is not a valid public key");
    let pub_key = &match pkey::PKey::from_ec_key(pub_ec_key.0) {
        Ok(key) => key,
        Err(err) => return Err(format!("Public Key Generation error: {:?}", err)),
    };
    let mut verifier = match Verifier::new(MessageDigest::sha256(), pub_key) {
        Ok(verifier) => verifier,
        Err(err) => return Err(format!("Verifier failed to initialize: {:?}", err)),
    };

    let data = &auth_token.t[0].clone().into_bytes();
    let verif_sig = base64::decode_config(
        &auth_token.t[1].clone().into_bytes(),
        base64::URL_SAFE_NO_PAD,
    )
    .expect("Signature failed to decode from base64");
    verifier
        .update(data)
        .expect("Data failed to load into verifier");

    // Extract the values from the combined raw key.
    let mut r_val = Vec::with_capacity(32);
    let mut s_val = Vec::with_capacity(32);
    r_val.extend(verif_sig[0..32].iter());
    s_val.extend(verif_sig[32..].iter());

    /* Compose the sequence DER by hand, because the current rust libraries lack this. */
    // write r & s as asn1
    // Prefix is the "\x02" + the length. We can cheat here because we know how long the keys are.
    let mut r_asn = vec![2];
    let mut s_asn = vec![2];
    // check if we need to pad for high order byte
    if r_val[0] > 127 {
        r_asn.extend_from_slice(&[33, 0])
    } else {
        r_asn.extend_from_slice(&[32])
    }
    r_asn.append(&mut r_val);
    if s_val[0] > 127 {
        s_asn.extend_from_slice(&[33, 0])
    } else {
        s_asn.extend_from_slice(&[32])
    }
    s_asn.append(&mut s_val);

    // seq = "\x30" + (len(rs) + len(ss)) + rs + ss
    let mut seq: Vec<u8> = vec![48];
    seq.append(&mut vec![(r_asn.len() + s_asn.len()) as u8]);
    seq.append(&mut r_asn);
    seq.append(&mut s_asn);

    match verifier.verify(&seq) {
        Ok(true) => {
            // Success! Return the decoded claims.
            let token = auth_token.t[0].clone();
            let claim_data: Vec<&str> = token.split(".").collect();
            let bytes = base64::decode_config(&claim_data[1], base64::URL_SAFE_NO_PAD)
                .expect("Claims were not properly base64 encoded");
            Ok(serde_json::from_str(
                &String::from_utf8(bytes)
                    .expect("Claims included an invalid character and could not be decoded."),
            )
            .expect("Claims are not valid JSON"))
        }
        Ok(false) => Err(format!("Verify failed")),
        Err(err) => Err(format!("Verify failed {:?}", err)),
    }
}

#[cfg(test)]
mod tests {
    use super::{*, Key};
    use std::collections::HashMap;


    fn test_claims() -> HashMap<String, serde_json::Value> {
        let reply: HashMap<String, serde_json::Value> = [
            (
                String::from("sub"),
                serde_json::Value::from("mailto:admin@example.com"),
            ),
            (String::from("exp"), serde_json::Value::from("1463001340")),
            (
                String::from("aud"),
                serde_json::Value::from("https://push.services.mozilla.com"),
            ),
        ]
        .iter()
        .cloned()
        .collect();
        reply
    }

    #[test]
    fn test_sign() {
        let key = Key::generate();
        let sub_val = serde_json::Value::from(String::from("mailto:mail@example.com"));

        let mut claims: HashMap<String, serde_json::Value> = HashMap::new();
        claims.insert(String::from("sub"), sub_val.clone());
        let result = sign(key, &mut claims).unwrap();
        let vresult = result.clone();

        // println!("{}", result);

        assert!(result.starts_with("Authorization: "));
        assert!(result.contains(" vapid "));

        // tear apart the auth token for the happy bits
        let token = result.split(" ").nth(2).unwrap();
        let sub_parts: Vec<&str> = token.split(",").collect();
        let mut auth_parts: HashMap<String, String> = HashMap::new();
        for kvi in &sub_parts {
            let kv: Vec<String> = kvi.splitn(2, "=").map(|x| String::from(x)).collect();
            auth_parts.insert(kv[0].clone(), kv[1].clone());
        }
        assert!(auth_parts.contains_key("t"));
        assert!(auth_parts.contains_key("k"));

        // now tear apart the token
        let token: Vec<&str> = auth_parts.get("t").unwrap().split(".").collect();
        assert_eq!(token.len(), 3);

        let content =
            String::from_utf8(base64::decode_config(token[0], base64::URL_SAFE_NO_PAD).unwrap())
                .unwrap();
        let items: HashMap<String, String> = serde_json::from_str(&content).unwrap();
        assert!(items.contains_key("typ"));
        assert!(items.contains_key("alg"));

        let content: String =
            String::from_utf8(base64::decode_config(token[1], base64::URL_SAFE_NO_PAD).unwrap())
                .unwrap();
        let items: HashMap<String, serde_json::Value> = serde_json::from_str(&content).unwrap();

        assert!(items.contains_key("exp"));
        assert!(items.contains_key("sub"));
        assert!(items.get("sub") == Some(&sub_val));

        // And verify that the signature works.
        // we do integration verify in `test_verify`
        verify(vresult).expect("Signed claims failed to self verify");
    }

    // TODO: Test fail cases, verification, values

    #[test]
    fn test_sign_bad_sub() {
        let key = Key::generate();
        let mut claims: HashMap<String, serde_json::Value> = HashMap::new();
        claims.insert(
            "sub".into(),
            serde_json::Value::from(String::from("invalid")),
        );
        match sign(key, &mut claims) {
            Ok(_) => panic!("Failed to reject invalid sub"),
            Err(err) => {
                assert!(err == String::from("\"sub\" not a valid HTML reference"));
            }
        }
    }

    #[test]
    fn test_sign_no_sub() {
        let key = Key::generate();
        let mut claims: HashMap<String, serde_json::Value> = HashMap::new();
        claims.insert(
            "blah".into(),
            serde_json::Value::from(String::from("mailto:a@b.c")),
        );
        match sign(key, &mut claims) {
            Ok(_) => panic!("Failed to reject missing sub"),
            Err(err) => {
                assert!(err == String::from("\"sub\" not found"));
            }
        }
    }

    #[test]
    fn test_verify_integration() {
        // Integration test with externally generated Authorization header.
        let test_header = [
            "Authorization: vapid t=eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJodHRwcz\
             ovL3B1c2guc2VydmljZXMubW96aWxsYS5jb20iLCJleHAiOiIxNDYzMDAxMzQwIiwic3ViIjoibWFp\
             bHRvOmFkbWluQGV4YW1wbGUuY29tIn0.4ZiULZaqZ8_7Cf2UYu7KO3eGaqZL5d4RZ6pwBvR0rcmTho\
             4WryVuZLfN-iMsHJ6Oc-4hkEZsMj8_32sXYSvTyg,k=BPD3F0hvy3Df69tjqRBN0ad08WH2nfaaxnp\
             kuIO6BV9Pa7p8xA8GauX0R_S-D-k82kcTNsCiJ6ML-zJisBpyybs",
        ]
        .join("");
        assert!(test_claims() == verify(test_header).unwrap())
    }

    //TODO: Add key input/output tests here.

}
