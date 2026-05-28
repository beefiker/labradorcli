const SSH_INTEGRATION_PROJECT_ENV: &str = "LABRADOR_SSH_INTEGRATION_TESTING_PROJECT";
const DEFAULT_SSH_INTEGRATION_PROJECT: &str = "labrador-ssh-integration-testing";
const SSH_INTEGRATION_HOST: &str = "ubuntu-14-04";
const SSH_INTEGRATION_PORT: &str = "25784";
const SSH_INTEGRATION_ZONE: &str = "us-east4-a";

/// The command used to proxy ssh requests through GCP's Identity-Aware Proxy.
fn proxy_command() -> String {
    let project = std::env::var(SSH_INTEGRATION_PROJECT_ENV)
        .unwrap_or_else(|_| DEFAULT_SSH_INTEGRATION_PROJECT.to_owned());

    format!(
        "gcloud compute start-iap-tunnel {SSH_INTEGRATION_HOST} {SSH_INTEGRATION_PORT} \
         --listen-on-stdin --project={project} --zone={SSH_INTEGRATION_ZONE}"
    )
}

/// Produces a user/host pair for testing a given remote shell.
pub fn user_host(shell: &str) -> String {
    format!("{shell}@{SSH_INTEGRATION_HOST}")
}

/// Produces the full ssh command to run to ssh into a given remote shell.
pub fn ssh_command(shell: &str, should_use_ssh_wrapper: bool) -> String {
    let proxy_command = proxy_command();
    [
        if should_use_ssh_wrapper {
            "ssh"
        } else {
            "command ssh"
        },
        &user_host(shell),
        &format!("-p {SSH_INTEGRATION_PORT}"),
        &format!("-o ProxyCommand=\"{proxy_command}\""),
        "-o StrictHostKeyChecking=no",
        "-o UserKnownHostsFile=/dev/null",
    ]
    .join(" ")
}
