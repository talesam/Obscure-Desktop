//! Maps core errors to short messages in the user's language.

use gettextrs::gettext;
use obscure_core::Error;

pub fn error_message(err: &Error) -> String {
    match err {
        Error::UnsupportedScheme(s) => {
            gettext("This kind of link is not supported yet: %s").replace("%s", s)
        }
        Error::MalformedLink(_) => gettext("The link looks incomplete or invalid."),
        Error::Http(e) if e.is_timeout() => gettext("The internet connection took too long."),
        Error::Http(e) if e.is_connect() => {
            gettext("Could not reach the internet to download the connection engine.")
        }
        Error::Http(_) | Error::NoAssetForArch(_) => {
            gettext("Could not download the connection engine. Try again later.")
        }
        Error::Checksum { .. } => gettext(
            "The downloaded file is corrupted. It will be downloaded again on the next attempt.",
        ),
        Error::Zip(_) => gettext("Could not extract the connection engine."),
        Error::CoreNotInstalled => gettext("The connection engine has not been downloaded yet."),
        Error::ConfigRejected(out) => {
            let hint = if out.contains("reality") || out.contains("publicKey") {
                gettext("Check the server's public key.")
            } else if out.contains("uuid") || out.contains("UUID") {
                gettext("The server identifier is not valid.")
            } else {
                gettext("Check the server details.")
            };
            format!(
                "{} {}",
                gettext("This server has a configuration the engine did not accept."),
                hint
            )
        }
        Error::CoreExited { output, .. } => {
            if output.contains("address already in use") {
                gettext(
                    "The local port is already in use by another program. Change it in Preferences.",
                )
            } else {
                gettext("The connection engine stopped unexpectedly.")
            }
        }
        Error::StartTimeout(_) => gettext("The connection engine did not respond in time."),
        Error::SysProxy(_) => {
            gettext("Could not change the system proxy. The local connection is still available.")
        }
        Error::DBus(_) => gettext("Could not notify the system about the new proxy."),
        Error::Io { .. } | Error::Json(_) => gettext("Could not read or write Obscure's files."),
        Error::Stats(_) => gettext("Could not read traffic statistics."),
        Error::Latency(_) => gettext("The server did not respond to the latency test."),
        Error::Subscription(_) => gettext("The subscription could not be read. Check the address."),
        Error::ClashYaml => {
            gettext("This subscription is in Clash format, which Obscure does not support yet.")
        }
    }
}
