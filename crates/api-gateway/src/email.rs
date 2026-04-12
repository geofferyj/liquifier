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

fn format_wei_as_ether(amount_wei: &str) -> String {
    let trimmed = amount_wei.trim();
    if trimmed.is_empty() {
        return "0".to_string();
    }

    let (is_negative, digits) = if let Some(stripped) = trimmed.strip_prefix('-') {
        (true, stripped)
    } else {
        (false, trimmed)
    };

    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return amount_wei.to_string();
    }

    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return "0".to_string();
    }

    const DECIMALS: usize = 18;
    let mut formatted = if digits.len() <= DECIMALS {
        let mut fractional = "0".repeat(DECIMALS - digits.len());
        fractional.push_str(digits);
        let fractional = fractional.trim_end_matches('0');
        if fractional.is_empty() {
            "0".to_string()
        } else {
            format!("0.{fractional}")
        }
    } else {
        let split = digits.len() - DECIMALS;
        let integer = &digits[..split];
        let fractional = digits[split..].trim_end_matches('0');
        if fractional.is_empty() {
            integer.to_string()
        } else {
            format!("{integer}.{fractional}")
        }
    };

    if is_negative {
        formatted.insert(0, '-');
    }

    formatted
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

    /// Send a deposit alert email to an admin.
    pub async fn send_deposit_alert(
        &self,
        admin_email: &str,
        username: &str,
        user_email: &str,
        wallet_address: &str,
        amount_wei: &str,
        token: &str,
        tx_hash: &str,
        chain: &str,
    ) {
        let amount_display = format_wei_as_ether(amount_wei);

        let body = format!(
            "New Deposit Alert\n\n\
             A common user has received a deposit:\n\n\
             User: {username} ({user_email})\n\
             Wallet: {wallet_address}\n\
             Amount: {amount_display}\n\
             Token: {token}\n\
             Chain: {chain}\n\
             Tx Hash: {tx_hash}\n\n\
             — Liquifier Platform"
        );

        let html_body = format!(
            r#"<!DOCTYPE html>
<html>
<body style="font-family: sans-serif; max-width: 600px; margin: 0 auto; padding: 20px;">
  <h2 style="color: #4F46E5;">New Deposit Detected</h2>
  <p>A common user has received a deposit into their wallet:</p>
  <table style="width: 100%; border-collapse: collapse; margin: 20px 0;">
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280; width: 120px;">User</td>
      <td style="padding: 8px 0; font-weight: 600;">{username}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Email</td>
      <td style="padding: 8px 0;">{user_email}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Wallet</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 13px;">{wallet_address}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Amount</td>
            <td style="padding: 8px 0; font-weight: 600;">{amount_display}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Token</td>
      <td style="padding: 8px 0;">{token}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Chain</td>
      <td style="padding: 8px 0;">{chain}</td>
    </tr>
    <tr>
      <td style="padding: 8px 0; color: #6b7280;">Tx Hash</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 12px; word-break: break-all;">{tx_hash}</td>
    </tr>
  </table>
  <p style="color: #999; font-size: 12px;">This is an automated alert from the Liquifier platform.</p>
</body>
</html>"#
        );

        let to_mailbox: Mailbox = match admin_email.parse() {
            Ok(m) => m,
            Err(e) => {
                warn!(email = %admin_email, error = %e, "Invalid admin email address");
                return;
            }
        };

        let subject = format!("Deposit Alert: {} received by {}", token, username);

        let email = match Message::builder()
            .from(self.from.clone())
            .to(to_mailbox)
            .subject(subject)
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
                warn!(error = %e, "Failed to build deposit alert email");
                return;
            }
        };

        match self.transport.send(email).await {
            Ok(_) => info!(email = %admin_email, "Deposit alert email sent"),
            Err(e) => warn!(email = %admin_email, error = %e, "Failed to send deposit alert email"),
        }
    }

    /// Send a trade/sale alert email to a common user.
    pub async fn send_trade_alert(
        &self,
        user_email: &str,
        username: &str,
        trade_id: &str,
        session_id: &str,
        chain: &str,
        sell_amount_wei: &str,
        received_amount_wei: &str,
        tx_hash: &str,
        status: &str,
        price_impact_bps: Option<f64>,
        failure_reason: Option<&str>,
    ) {
        let sell_amount_display = format_wei_as_ether(sell_amount_wei);
        let received_amount_display = format_wei_as_ether(received_amount_wei);

        let impact_display = price_impact_bps
            .map(|b| format!("{:.2} bps", b))
            .unwrap_or_else(|| "N/A".to_string());

        let status_label = match status {
            "completed" => "Completed",
            "failed" => "Failed",
            _ => status,
        };

        let failure_line = failure_reason
            .map(|r| format!("Failure Reason: {}\n", r))
            .unwrap_or_default();

        let body = format!(
            "Trade Executed\n\n\
             Hi {username},\n\n\
             A trade has been executed on your session:\n\n\
             Trade ID: {trade_id}\n\
             Session ID: {session_id}\n\
             Chain: {chain}\n\
             Sell Amount: {sell_amount_display}\n\
             Received Amount: {received_amount_display}\n\
             Price Impact: {impact_display}\n\
             Status: {status_label}\n\
             {failure_line}\
             Tx Hash: {tx_hash}\n\n\
             — Liquifier Platform"
        );

        let status_color = if status == "completed" {
            "#16a34a"
        } else {
            "#dc2626"
        };

        let failure_row = failure_reason
            .map(|r| {
                format!(
                    r#"<tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Reason</td>
      <td style="padding: 8px 0; color: #dc2626;">{}</td>
    </tr>"#,
                    r
                )
            })
            .unwrap_or_default();

        let html_body = format!(
            r#"<!DOCTYPE html>
<html>
<body style="font-family: sans-serif; max-width: 600px; margin: 0 auto; padding: 20px;">
  <h2 style="color: #4F46E5;">Trade Executed</h2>
  <p>Hi {username}, a trade has been executed on your session:</p>
  <table style="width: 100%; border-collapse: collapse; margin: 20px 0;">
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280; width: 140px;">Trade ID</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 13px;">{trade_id}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Session ID</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 13px;">{session_id}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Chain</td>
      <td style="padding: 8px 0;">{chain}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Sell Amount</td>
            <td style="padding: 8px 0; font-weight: 600;">{sell_amount_display}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Received Amount</td>
            <td style="padding: 8px 0; font-weight: 600;">{received_amount_display}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Price Impact</td>
      <td style="padding: 8px 0;">{impact_display}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Status</td>
      <td style="padding: 8px 0; font-weight: 600; color: {status_color};">{status_label}</td>
    </tr>
    {failure_row}
    <tr>
      <td style="padding: 8px 0; color: #6b7280;">Tx Hash</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 12px; word-break: break-all;">{tx_hash}</td>
    </tr>
  </table>
  <p style="color: #999; font-size: 12px;">This is an automated alert from the Liquifier platform.</p>
</body>
</html>"#
        );

        let to_mailbox: Mailbox = match user_email.parse() {
            Ok(m) => m,
            Err(e) => {
                warn!(email = %user_email, error = %e, "Invalid user email address for trade alert");
                return;
            }
        };

        let subject = format!("Trade {} — {}", status_label, chain);

        let email = match Message::builder()
            .from(self.from.clone())
            .to(to_mailbox)
            .subject(subject)
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
                warn!(error = %e, "Failed to build trade alert email");
                return;
            }
        };

        match self.transport.send(email).await {
            Ok(_) => info!(email = %user_email, trade_id = %trade_id, "Trade alert email sent"),
            Err(e) => warn!(email = %user_email, error = %e, "Failed to send trade alert email"),
        }
    }

    /// Send a deposit alert email to the common user who received the deposit.
    pub async fn send_user_deposit_alert(
        &self,
        user_email: &str,
        username: &str,
        wallet_address: &str,
        amount_wei: &str,
        token: &str,
        tx_hash: &str,
        chain: &str,
    ) {
        let amount_display = format_wei_as_ether(amount_wei);

        let body = format!(
            "Deposit Received\n\n\
             Hi {username},\n\n\
             A deposit has been received into your wallet:\n\n\
             Wallet: {wallet_address}\n\
             Amount: {amount_display}\n\
             Token: {token}\n\
             Chain: {chain}\n\
             Tx Hash: {tx_hash}\n\n\
             — Liquifier Platform"
        );

        let html_body = format!(
            r#"<!DOCTYPE html>
<html>
<body style="font-family: sans-serif; max-width: 600px; margin: 0 auto; padding: 20px;">
  <h2 style="color: #4F46E5;">Deposit Received</h2>
  <p>Hi {username}, a deposit has been received into your wallet:</p>
  <table style="width: 100%; border-collapse: collapse; margin: 20px 0;">
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280; width: 120px;">Wallet</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 13px;">{wallet_address}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Amount</td>
      <td style="padding: 8px 0; font-weight: 600;">{amount_display}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Token</td>
      <td style="padding: 8px 0;">{token}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Chain</td>
      <td style="padding: 8px 0;">{chain}</td>
    </tr>
    <tr>
      <td style="padding: 8px 0; color: #6b7280;">Tx Hash</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 12px; word-break: break-all;">{tx_hash}</td>
    </tr>
  </table>
  <p style="color: #999; font-size: 12px;">This is an automated alert from the Liquifier platform.</p>
</body>
</html>"#
        );

        let to_mailbox: Mailbox = match user_email.parse() {
            Ok(m) => m,
            Err(e) => {
                warn!(email = %user_email, error = %e, "Invalid user email address for deposit alert");
                return;
            }
        };

        let subject = format!("Deposit Received — {} on {}", token, chain);

        let email = match Message::builder()
            .from(self.from.clone())
            .to(to_mailbox)
            .subject(subject)
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
                warn!(error = %e, "Failed to build user deposit alert email");
                return;
            }
        };

        match self.transport.send(email).await {
            Ok(_) => info!(email = %user_email, "User deposit alert email sent"),
            Err(e) => {
                warn!(email = %user_email, error = %e, "Failed to send user deposit alert email")
            }
        }
    }

    /// Send a shortfall alert to a common user when the sale didn't cover the minimum amount.
    pub async fn send_shortfall_alert(
        &self,
        user_email: &str,
        username: &str,
        session_id: &str,
        received_usd: f64,
        required_usd: f64,
        shortfall_usd: f64,
    ) {
        let body = format!(
            "Additional Deposit Required\n\n\
             Hi {username},\n\n\
             Your selling session (ID: {session_id}) has completed, but the realised \
             USDT value (${received_usd:.2}) fell short of the minimum deposit \
             amount (${required_usd:.2}).\n\n\
             Please deposit an additional ${shortfall_usd:.2} worth of tokens \
             to bring your total up to the required minimum.\n\n\
             — Liquifier Platform"
        );

        let html_body = format!(
            r#"<!DOCTYPE html>
<html>
<body style="font-family: sans-serif; max-width: 600px; margin: 0 auto; padding: 20px;">
  <h2 style="color: #dc2626;">Additional Deposit Required</h2>
  <p>Hi {username},</p>
  <p>Your selling session has completed, but the realised USDT value fell short of the required amount:</p>
  <table style="width: 100%; border-collapse: collapse; margin: 20px 0;">
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280; width: 160px;">Session ID</td>
      <td style="padding: 8px 0; font-family: monospace; font-size: 13px;">{session_id}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Realised USDT</td>
      <td style="padding: 8px 0; font-weight: 600;">${received_usd:.2}</td>
    </tr>
    <tr style="border-bottom: 1px solid #e5e7eb;">
      <td style="padding: 8px 0; color: #6b7280;">Required Minimum</td>
      <td style="padding: 8px 0; font-weight: 600;">${required_usd:.2}</td>
    </tr>
    <tr>
      <td style="padding: 8px 0; color: #6b7280;">Amount to Deposit</td>
      <td style="padding: 8px 0; font-weight: 600; color: #dc2626;">${shortfall_usd:.2}</td>
    </tr>
  </table>
  <p>Please top up your wallet with the shortfall amount to continue.</p>
  <p style="color: #999; font-size: 12px;">This is an automated alert from the Liquifier platform.</p>
</body>
</html>"#
        );

        let to_mailbox: Mailbox = match user_email.parse() {
            Ok(m) => m,
            Err(e) => {
                warn!(email = %user_email, error = %e, "Invalid user email for shortfall alert");
                return;
            }
        };

        let subject = format!("Action Required: Additional ${shortfall_usd:.2} Deposit Needed");

        let email = match Message::builder()
            .from(self.from.clone())
            .to(to_mailbox)
            .subject(subject)
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
                warn!(error = %e, "Failed to build shortfall alert email");
                return;
            }
        };

        match self.transport.send(email).await {
            Ok(_) => {
                info!(email = %user_email, session_id = %session_id, "Shortfall alert email sent")
            }
            Err(e) => {
                warn!(email = %user_email, error = %e, "Failed to send shortfall alert email")
            }
        }
    }
}
