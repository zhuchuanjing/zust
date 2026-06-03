use anyhow::{Result, anyhow};
use dynamic::{Dynamic, map};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const OSS_PREFIX: &str = "oss:://";

struct OssConfig {
    access_id: String,
    access_key: String,
    region: String,
    bucket: String,
    date: String,
    sign_key: [u8; 32],
}

impl OssConfig {
    fn from_dynamic(config: Dynamic) -> Result<Self> {
        if !config.is_map() {
            return Err(anyhow!("oss config must be map"));
        }
        let access_id = required_string(&config, "access_id")?;
        let access_key = required_string(&config, "access_key")?;
        let region = required_string(&config, "region")?;
        let bucket = required_string(&config, "bucket")?;
        let time_stamp = oss_time_stamp();
        let date = time_stamp[..8].to_string();
        let sign_key = signing_key(&access_key, &region, &date);
        Ok(Self { access_id, access_key, region, bucket, date, sign_key })
    }

    async fn upload(&mut self, object_name: &str, data: Vec<u8>) -> Result<()> {
        let time_stamp = oss_time_stamp();
        let content_type = content_type_for_object(object_name);
        let url = format!("https://{}.oss-{}.aliyuncs.com/{}", self.bucket, self.region, object_name);
        let sign = self.signature("PUT", &format!("/{}/{}", self.bucket, object_name), data.len() as u64, content_type, &time_stamp);
        let auth = format!("OSS4-HMAC-SHA256 Credential={}/{}/{}/oss/aliyun_v4_request, AdditionalHeaders=content-disposition;content-length, Signature={}", self.access_id, self.date, self.region, sign);

        let response = reqwest::Client::builder()
            .no_proxy()
            .build()?
            .put(url)
            .header("content-type", content_type)
            .header("content-length", data.len().to_string())
            .header("content-disposition", "attachment")
            .header("x-oss-content-sha256", "UNSIGNED-PAYLOAD")
            .header("x-oss-date", time_stamp)
            .header("Authorization", auth)
            .body(data)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("oss upload failed: {status} {body}"));
        }

        Ok(())
    }

    fn get_link(&mut self, object_name: &str, expires_seconds: u32) -> String {
        let time_stamp = oss_time_stamp();
        self.update_sign_key(&time_stamp);
        let request = format!(
            r#"GET
/{}/{}
x-oss-additional-headers=host&x-oss-credential={}%2F{}%2F{}%2Foss%2Faliyun_v4_request&x-oss-date={}&x-oss-expires={}&x-oss-signature-version=OSS4-HMAC-SHA256
host:{}.oss-{}.aliyuncs.com

host
UNSIGNED-PAYLOAD"#,
            self.bucket, object_name, self.access_id, self.date, self.region, time_stamp, expires_seconds, self.bucket, self.region
        );
        let request_hmac = hex_string(&sha256(request.as_bytes()));
        let sign_str = format!("OSS4-HMAC-SHA256\n{}\n{}/{}/oss/aliyun_v4_request\n{}", time_stamp, self.date, self.region, request_hmac);
        let sign = hex_string(&hmac_sha256(self.sign_key.as_slice(), sign_str.as_bytes()));
        format!(
            "https://{}.oss-{}.aliyuncs.com/{}?x-oss-additional-headers=host&x-oss-credential={}%2F{}%2F{}%2Foss%2Faliyun_v4_request&x-oss-date={}&x-oss-expires={}&x-oss-signature={}&x-oss-signature-version=OSS4-HMAC-SHA256",
            self.bucket, self.region, object_name, self.access_id, self.date, self.region, time_stamp, expires_seconds, sign
        )
    }

    fn update_sign_key(&mut self, time_stamp: &str) {
        let Some((date, _)) = time_stamp.split_once('T') else {
            return;
        };
        if date != self.date {
            self.date = date.to_string();
            self.sign_key = signing_key(&self.access_key, &self.region, &self.date);
        }
    }

    fn signature(&mut self, method: &str, uri: &str, length: u64, content_type: &str, time_stamp: &str) -> String {
        self.update_sign_key(time_stamp);
        let request = format!(
            r#"{}
{}

content-disposition:attachment
content-length:{}
content-type:{}
x-oss-content-sha256:UNSIGNED-PAYLOAD
x-oss-date:{}

content-disposition;content-length
UNSIGNED-PAYLOAD"#,
            method, uri, length, content_type, time_stamp
        );
        let request_hmac = hex_string(&sha256(request.as_bytes()));
        let sign_str = format!("OSS4-HMAC-SHA256\n{}\n{}/{}/oss/aliyun_v4_request\n{}", time_stamp, self.date, self.region, request_hmac);
        hex_string(&hmac_sha256(self.sign_key.as_slice(), sign_str.as_bytes()))
    }
}

pub async fn upload(config: Dynamic, object_name: &str, data: Vec<u8>) -> Result<String> {
    let mut config = OssConfig::from_dynamic(config)?;
    config.upload(object_name, data).await?;
    Ok(format!("{OSS_PREFIX}{object_name}"))
}

pub fn get_link(config: Dynamic, oss_url: &str, expires_seconds: u32) -> Result<String> {
    let object_name = oss_url.strip_prefix(OSS_PREFIX).ok_or_else(|| anyhow!("invalid oss url"))?;
    let mut config = OssConfig::from_dynamic(config)?;
    Ok(config.get_link(object_name, expires_seconds))
}

pub fn signed_url_request(config: Dynamic, req: Dynamic) -> Dynamic {
    match signed_url_request_result(config, req) {
        Ok(result) => result,
        Err(err) => map!("ok"=> false, "error"=> err.to_string()),
    }
}

fn signed_url_request_result(config: Dynamic, req: Dynamic) -> Result<Dynamic> {
    let (expires_seconds, oss_url) = if req.is_map() {
        let expires_seconds = req.get_dynamic("expires").or_else(|| req.get_dynamic("expires_seconds")).and_then(|value| value.as_int()).unwrap_or(600).max(1) as u32;
        let oss_url = req.get_dynamic("oss_url").or_else(|| req.get_dynamic("ossUrl")).or_else(|| req.get_dynamic("url")).map(|value| value.as_str().to_string());
        (expires_seconds, oss_url)
    } else {
        (600, Some(req.as_str().to_string()))
    };
    let oss_url = oss_url.filter(|value| value.starts_with(OSS_PREFIX)).ok_or_else(|| anyhow!("oss_url missing or invalid"))?;
    let url = get_link(config, &oss_url, expires_seconds)?;
    Ok(map!("ok"=> true, "oss_url"=> oss_url, "url"=> url))
}

fn required_string(config: &Dynamic, key: &str) -> Result<String> {
    config.get_dynamic(key).map(|value| value.as_str().to_string()).filter(|value| !value.is_empty()).ok_or_else(|| anyhow!("missing oss config field {key}"))
}

fn signing_key(key: &str, region: &str, date: &str) -> [u8; 32] {
    let date_key = hmac_sha256(format!("aliyun_v4{key}").as_bytes(), date.as_bytes());
    let date_region_key = hmac_sha256(&date_key, region.as_bytes());
    let date_region_service_key = hmac_sha256(&date_region_key, b"oss");
    hmac_sha256(&date_region_service_key, b"aliyun_v4_request")
}

fn oss_time_stamp() -> String {
    let zoned = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", zoned.year(), zoned.month(), zoned.day(), zoned.hour(), zoned.minute(), zoned.second())
}

fn content_type_for_object(object_name: &str) -> &'static str {
    match object_name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("webm") => "audio/webm",
        Some("mp4") => "video/mp4",
        _ => "application/octet-stream",
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hmac_sha256(key: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key size");
    mac.update(bytes);
    mac.finalize().into_bytes().into()
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_uses_extension() {
        assert_eq!(content_type_for_object("voice.wav"), "audio/wav");
        assert_eq!(content_type_for_object("dump.bin"), "application/octet-stream");
    }
}
