//! Explicit reference profile: maajiko/Epay@a4d0f042 (not universal vendor compatibility).
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use conduit_config::model::PaymentConfig;
use md5::{Digest, Md5};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct EpayGateway {
    config: PaymentConfig,
    client: reqwest::Client,
}
pub(crate) struct QueryPayment {
    pub paid: bool,
    pub merchant_id: String,
    pub order_id: String,
    pub transaction_id: String,
    pub channel: String,
    pub amount_minor: i64,
    pub paid_at: Option<DateTime<Utc>>,
}
impl EpayGateway {
    pub fn new(config: PaymentConfig) -> Result<Self, String> {
        let url =
            url::Url::parse(&config.gateway_base_url).map_err(|_| "payment gateway URL invalid")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !config.gateway_base_url.ends_with('/')
        {
            return Err("payment gateway requires fixed credential-free HTTPS base URL".into());
        }
        Self::build(config)
    }
    #[cfg(test)]
    pub fn fixture(config: PaymentConfig) -> Result<Self, String> {
        Self::build(config)
    }
    fn build(config: PaymentConfig) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15));
        if let Some(pem) = &config.gateway_ca_certificate {
            if pem.len() > 65536 {
                return Err("payment gateway CA exceeds limit".into());
            }
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(pem.as_bytes())
                    .map_err(|_| "payment gateway CA invalid")?,
            );
        }
        let client = builder
            .build()
            .map_err(|_| "payment HTTP client unavailable")?;
        Ok(Self { config, client })
    }
    pub fn snapshot(&self) -> String {
        serde_json::json!({"profile":self.config.profile,"gateway":self.config.gateway_base_url,"merchantID":self.config.merchant_id,"notifyURL":self.config.notify_url,"returnURL":self.config.return_url}).to_string()
    }
    pub fn identity(&self) -> &str {
        &self.config.merchant_identity
    }
    pub fn merchant_id(&self) -> &str {
        &self.config.merchant_id
    }
    pub fn checkout(&self, id: Uuid, amount: i64, channel: &str) -> Result<String, String> {
        let mut fields = BTreeMap::from([
            ("pid".into(), self.config.merchant_id.clone()),
            ("out_trade_no".into(), id.to_string()),
            ("type".into(), provider_channel(channel)?.into()),
            ("notify_url".into(), self.config.notify_url.clone()),
            ("return_url".into(), self.config.return_url.clone()),
            ("name".into(), "Conduit API Project credits".into()),
            (
                "money".into(),
                format!("{}.{:02}", amount / 100, amount % 100),
            ),
        ]);
        fields.insert("sign".into(), signature(&fields, &self.config.merchant_key));
        fields.insert("sign_type".into(), "MD5".into());
        let mut url = url::Url::parse(&self.config.gateway_base_url)
            .and_then(|u| u.join("submit.php"))
            .map_err(|_| "payment gateway URL invalid")?;
        url.query_pairs_mut().extend_pairs(fields);
        Ok(url.to_string())
    }
    pub fn notify(&self, raw: &str) -> Result<BTreeMap<String, String>, String> {
        let fields = parse_query(raw)?;
        if fields.get("sign_type").is_none_or(|v| v != "MD5") {
            return Err("invalid payment signature type".into());
        }
        let actual = fields.get("sign").ok_or("payment signature missing")?;
        let expected = signature(&fields, &self.config.merchant_key);
        if actual.len() != 32
            || actual
                .bytes()
                .zip(expected.bytes())
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                != 0
        {
            return Err("invalid payment signature".into());
        }
        if fields
            .get("trade_status")
            .is_none_or(|v| v != "TRADE_SUCCESS")
        {
            return Err("payment notification is not successful".into());
        }
        Ok(fields)
    }
    pub async fn query(&self, id: Uuid) -> Result<QueryPayment, String> {
        let mut url = url::Url::parse(&self.config.gateway_base_url)
            .and_then(|u| u.join("api.php"))
            .map_err(|_| "payment gateway URL invalid")?;
        url.query_pairs_mut().extend_pairs([
            ("act", "order"),
            ("pid", &self.config.merchant_id),
            ("key", &self.config.merchant_key),
            ("out_trade_no", &id.to_string()),
        ]);
        // Never format reqwest errors: they include the credential-bearing URL.
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| "payment query transport failed")?;
        if !response.status().is_success() {
            return Err("payment query HTTP failure".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "payment query body failed")?
        {
            if bytes.len() + chunk.len() > 65536 {
                return Err("payment query body exceeds limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "payment query invalid JSON")?;
        if scalar(&value, "code")? != "1" {
            return Err("payment query rejected".into());
        }
        let paid = scalar(&value, "status")? == "1";
        let paid_at = if paid {
            let raw = scalar(&value, "endtime")?;
            let local = NaiveDateTime::parse_from_str(&raw, "%Y-%m-%d %H:%M:%S")
                .map_err(|_| "payment query paid time invalid")?;
            Some(
                chrono_tz::Asia::Shanghai
                    .from_local_datetime(&local)
                    .single()
                    .ok_or("payment query paid time ambiguous")?
                    .with_timezone(&Utc),
            )
        } else {
            None
        };
        Ok(QueryPayment {
            paid,
            merchant_id: scalar(&value, "pid")?,
            order_id: scalar(&value, "out_trade_no")?,
            transaction_id: scalar(&value, "trade_no")?,
            channel: local_channel(&scalar(&value, "type")?)
                .unwrap_or("unrecognized")
                .into(),
            amount_minor: money_minor(&scalar(&value, "money")?)?,
            paid_at,
        })
    }
}
fn scalar(value: &Value, key: &str) -> Result<String, String> {
    match value.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Ok(n.to_string()),
        _ => Err("payment query field missing or invalid".into()),
    }
}
pub(crate) fn provider_channel(channel: &str) -> Result<&'static str, String> {
    match channel {
        "alipay" => Ok("alipay"),
        "wechat" => Ok("wxpay"),
        _ => Err("unsupported payment channel".into()),
    }
}
pub(crate) fn local_channel(channel: &str) -> Result<&'static str, String> {
    match channel {
        "alipay" => Ok("alipay"),
        "wxpay" => Ok("wechat"),
        _ => Err("unsupported payment channel".into()),
    }
}
pub(crate) fn money_minor(raw: &str) -> Result<i64, String> {
    let mut pieces = raw.split('.');
    let whole = pieces.next().unwrap_or_default();
    let fraction = pieces.next();
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || pieces.next().is_some()
        || whole.len() > 17
    {
        return Err("payment amount invalid".into());
    }
    let cents = match fraction {
        None => 0,
        Some(f) if (1..=2).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()) => {
            f.parse::<i64>().map_err(|_| "payment amount invalid")?
                * if f.len() == 1 { 10 } else { 1 }
        }
        _ => return Err("payment amount invalid".into()),
    };
    whole
        .parse::<i64>()
        .ok()
        .and_then(|v| v.checked_mul(100))
        .and_then(|v| v.checked_add(cents))
        .filter(|v| *v > 0)
        .ok_or("payment amount invalid".into())
}
pub(crate) fn signature(fields: &BTreeMap<String, String>, secret: &str) -> String {
    let prehash = fields
        .iter()
        .filter(|(k, v)| {
            k.as_str() != "sign"
                && k.as_str() != "sign_type"
                && !v
                    .trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\0' | '\x0b'))
                    .is_empty()
        })
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let mut hash = Md5::new();
    hash.update(prehash.as_bytes());
    hash.update(secret.as_bytes());
    format!("{:x}", hash.finalize())
}
pub(crate) fn parse_query(raw: &str) -> Result<BTreeMap<String, String>, String> {
    if raw.is_empty() || raw.len() > 16384 {
        return Err("payment query size invalid".into());
    }
    let mut result = BTreeMap::new();
    for pair in raw.split('&') {
        if result.len() >= 32 {
            return Err("too many payment fields".into());
        }
        let (key, value) = pair.split_once('=').ok_or("invalid payment field")?;
        let key = decode(key)?;
        let value = decode(value)?;
        if !key.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || result.insert(key, value).is_some()
        {
            return Err("duplicate or invalid payment field".into());
        }
    }
    Ok(result)
}
fn decode(raw: &str) -> Result<String, String> {
    let mut output = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => output.push(b' '),
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err("invalid payment encoding".into());
                }
                let hex = |b: u8| match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                };
                output.push(
                    hex(bytes[i + 1])
                        .zip(hex(bytes[i + 2]))
                        .map(|(a, b)| a * 16 + b)
                        .ok_or("invalid payment encoding")?,
                );
                i += 2;
            }
            b => output.push(b),
        };
        i += 1;
    }
    String::from_utf8(output).map_err(|_| "invalid payment UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn derived_reference_signing_vector_and_strict_parsing() {
        let map = BTreeMap::from([
            ("a".into(), "0".into()),
            ("b".into(), " 空+%&=".into()),
            ("c".into(), "中文".into()),
            ("empty".into(), "".into()),
            ("blank".into(), " ".into()),
            ("sign".into(), "ignored".into()),
            ("sign_type".into(), "MD5".into()),
        ]);
        assert_eq!(
            signature(&map, "test-key"),
            "1fe0ed95a23f863a44c705b0b5b412de"
        );
        for invalid in [
            "a=1&a=2", "a[]=1", "a=%FF", "a=%Z0", "a=%", "a.b=1", "a+b=1",
        ] {
            assert!(parse_query(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            parse_query("a=0&b=%E4%B8%AD%E6%96%87+%2B%25%26%3D").unwrap()["b"],
            "中文 +%&="
        );
        assert_eq!(money_minor("0.01").unwrap(), 1);
        for invalid in [
            "0.001",
            "1e2",
            "-1",
            "+1",
            "1.",
            "0",
            "92233720368547758.08",
        ] {
            assert!(money_minor(invalid).is_err(), "{invalid}");
        }
        let config = PaymentConfig {
            gateway_base_url: "https://gateway.example.test/".into(),
            merchant_key: "test-key".into(),
            ..Default::default()
        };
        let gateway = EpayGateway::new(config.clone()).unwrap();
        let mut fields = BTreeMap::from([
            ("trade_status".into(), "TRADE_SUCCESS".into()),
            ("sign_type".into(), "MD5".into()),
        ]);
        fields.insert("sign".into(), signature(&fields, "test-key"));
        let encode = |f: &BTreeMap<String, String>| {
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(f)
                .finish()
        };
        assert!(gateway.notify(&encode(&fields)).is_ok());
        fields.insert("sign".into(), fields["sign"].to_ascii_uppercase());
        assert!(gateway.notify(&encode(&fields)).is_err());
        let mut insecure = config;
        insecure.gateway_base_url = "http://127.0.0.1/".into();
        assert!(EpayGateway::new(insecure).is_err());
    }
}
