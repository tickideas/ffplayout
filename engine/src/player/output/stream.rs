use std::process::Stdio;

use log::*;
use tokio::process::{Child, Command};

use crate::utils::{
    config::PlayoutConfig,
    logging::{Target, fmt_cmd},
};
use crate::vec_strings;
use crate::{
    player::{
        controller::ProcessUnit::*,
        utils::{Media, insert_readrate, prepare_output_cmd},
    },
    utils::errors::ServiceError,
};

/// Streaming Output
///
/// Prepare the ffmpeg command for streaming output
pub async fn output(config: &PlayoutConfig, log_format: &str) -> Result<Child, ServiceError> {
    let id = config.general.channel_id;
    let mut enc_prefix = vec_strings!["-hide_banner", "-nostats", "-v", log_format];
    let mut media = Media {
        unit: Encoder,
        ..Default::default()
    };
    media.add_filter(config, &None).await;

    if let Some(input_cmd) = &config.advanced.encoder.input_cmd {
        enc_prefix.append(&mut input_cmd.clone());
    }

    enc_prefix.append(&mut vec_strings!["-i", "pipe:0"]);

    insert_readrate(&config.general.ffmpeg_options, &mut enc_prefix, 1.0);

    let enc_cmd = prepare_output_cmd(config, enc_prefix, &media.filter);

    debug!(target: Target::file_mail(), channel = id;
        "Encoder CMD: <span class=\"log-cmd\">ffmpeg {}</span>",
        fmt_cmd(&enc_cmd)
    );

    let child = Command::new("ffmpeg")
        .args(enc_cmd)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    Ok(child)
}
