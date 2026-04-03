use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use tracing::{info, warn};

#[derive(Clone)]
pub struct EmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    base_url: String,
}

impl EmailSender {
    pub fn new(cfg: &liquifier_config::SmtpSettings) -> Option<Self> {
        if cfg.host.is_empty() {
            warn!("SMTP host not configured — emails will be skipped (tokens logged only)");
            return None;
        }

        let creds = Credentials::new(cfg.username.clone(), cfg.password.clone());

        let transport = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)
            .expect("Failed to create SMTP transport")
            .port(cfg.port)
            .credentials(creds)
            .build();

        let from_name = if cfg.from_name.is_empty() {
            "Liquifier".to_string()
        } else {
            cfg.from_name.clone()
        };

        let from: Mailbox = format!("{} <{}>", from_name, cfg.from_email)
            .parse()
            .expect("Invalid from_email in SMTP config");

        Some(Self {
            transport,
            from,
            base_url: cfg.base_url.clone(),
        })
    }

    pub async fn send_verification_email(&self, to_email: &str, token: &str) {
        let verify_url = format!("{}/verify-email?token={}", self.base_url, token);

        let body = format!(
            "Welcome to Liquifier!\n\n\
             Please verify your email by clicking the link below:\n\n\
             {}\n\n\
             This link expires in 24 hours.\n\n\
             If you didn't create an account, you can safely ignore this email.",
            verify_url
        );

        let html_body = format!(
            r#"<!DOCTYPE html>
<html>
<body style="font-family: sans-serif; max-width: 600px; margin: 0 auto; padding: 20px;">
  <h2>Welcome to Liquifier!</h2>
  <p>Please verify your email by clicking the button below:</p>
  <p style="text-align: center; margin: 30px 0;">
    <a href="{url}" style="background-color: #4F46E5; color: white; padding: 12px 24px; text-decoration: none; border-radius: 6px; font-weight: bold;">
      Verify Email
    </a>
  </p>
  <p style="color: #666; font-size: 14px;">Or copy this link: <a href="{url}">{url}</a></p>
  <p style="color: #666; font-size: 14px;">This link expires in 24 hours.</p>
  <p style="color: #999; font-size: 12px;">If you didn't create an account, you can safely ignore this email.</p>
</body>
</html>"#,
            url = verify_url
        );

        let to_mailbox: Mailbox = match to_email.parse() {
            Ok(m) => m,
            Err(e) => {
                warn!(email = %to_email, error = %e, "Invalid recipient email address");
                return;
            }
        };

        let email = match Message::builder()
            .from(self.from.clone())
            .to(to_mailbox)
            .subject("Verify your Liquifier account")
            .multipart(
                lettre::message::MultiPart::alternative()
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(body),
                    )
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html_body),
                    ),
            ) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Failed to build verification email");
                return;
            }
        };

        match self.transport.send(email).await {
            Ok(_) => info!(email = %to_email, "Verification email sent"),
            Err(e) => warn!(email = %to_email, error = %e, "Failed to send verification email"),
        }
    }
}
