# linear-webhook

Receives [Linear webhooks](https://linear.app/developers/webhooks) and forwards them to Discord webhooks.

Each delivery becomes a Discord embed containing the title, actor, link, description, state, priority, team, assignee, labels and so on, plus a field listing what changed in an update. The exact request body is attached as a `.json.zst` file, so nothing is lost when the embed has to be truncated to fit Discord's limits:

```sh
zstd -dc linear-Issue-update-<delivery>.json.zst | jq
```

## Configuration

```toml
# Served at POST /server1
[server1]
discord_webhook_url = "https://discord.com/api/webhooks/<id>/<token>"
linear_secret = "lin_wh_..."

# Values can also be read from files (never both for one key).
[server2]
discord_webhook_url_file = "/run/secrets/server2-discord"
linear_secret_file = "/run/secrets/server2-linear"
```

Server names must be 1–64 characters of `[A-Za-z0-9_-]`. Point the Linear webhook at `https://<your host>/<name>`.

## Running

```sh
linear-webhook --config config.toml --address 127.0.0.1 --port 9999
```

| Response | Meaning |
|---|---|
| 200 | Forwarded to Discord |
| 400 | Malformed payload or `Linear-Delivery` header |
| 401 | Bad or missing `Linear-Signature`, or `webhookTimestamp` more than 60 s away from now |
| 404 | Unknown server name |
| 413 | Body over 1 MiB |
| 502 | Discord failed or took longer than 4 s; Linear will retry |

### Local testing

```sh
# Without secrets: no signature or timestamp checks. linear_secret may be omitted.
linear-webhook -c config.toml --dangerously-ignore-secret
scripts/send-test.sh http://127.0.0.1:9999/server1 examples/issue-update.json

# With secrets: the script signs the body like Linear does.
linear-webhook -c config.toml
scripts/send-test.sh http://127.0.0.1:9999/server1 examples/issue-update.json "$LINEAR_SECRET"
```

`send-test.sh` sets `webhookTimestamp` to the current time so the replay check passes.

## NixOS

```nix
{
  inputs.linear-webhook.url = "github:witchof0x20/linear-webhook";

  outputs = { nixpkgs, linear-webhook, ... }: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      modules = [
        linear-webhook.nixosModules.default
        {
          services.linear-webhook = {
            enable = true;
            listenHost = "127.0.0.1";
            listenPort = 9999;
            servers = {
              server1 = {
                discordWebhookUrlFile = "/run/agenix/server1-discord";
                linearSecretFile = "/run/agenix/server1-linear";
              };
              # Inline values work too, but end up in the world-readable Nix store.
              server2 = {
                discordWebhookUrl = "https://discord.com/api/webhooks/...";
                linearSecret = "lin_wh_...";
              };
            };
          };
        }
      ];
    };
  };
}
```

`*File` secrets are passed in through systemd `LoadCredential=`, so they can stay `root:root 0400`. The service runs as a `DynamicUser` with the usual systemd sandboxing. Other options are `openFirewall`, `dangerouslyIgnoreSecret`, `extraArgs` and `package`.

Linear only delivers to HTTPS endpoints, so put this behind a reverse proxy, for example:

```nix
services.caddy.virtualHosts."hooks.example.com".extraConfig = ''
  reverse_proxy 127.0.0.1:9999
'';
```

## Development

```sh
nix develop
cargo test                          # unit and integration tests (fake Discord)
nix flake check                     # package, clippy, and the NixOS VM test
```
