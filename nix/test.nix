{ self }:
{ pkgs, lib, ... }:

let
  # Token-shaped strings so we can grep the journal for leaks.
  token1 = lib.concatStrings (lib.replicate 68 "a");
  token2 = lib.concatStrings (lib.replicate 68 "b");
  webhook = id: token: "https://discord.com/api/webhooks/${id}/${token}";

  fixtures = ../examples;

  send-test = pkgs.writeShellApplication {
    name = "send-test";
    runtimeInputs = [ pkgs.curl pkgs.openssl pkgs.gnused pkgs.coreutils ];
    text = builtins.readFile ../scripts/send-test.sh;
  };

  # Answers every POST with 204 and saves the request for inspection.
  fake-discord = pkgs.writers.writePython3 "fake-discord" { } ''
    import os
    import time
    from http.server import BaseHTTPRequestHandler, HTTPServer

    OUT = "/var/lib/fake-discord"


    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers["Content-Length"]))
            path = os.path.join(OUT, str(time.time_ns()))
            with open(path, "wb") as f:
                f.write(self.path.encode() + b"\n" + body)
            self.send_response(204)
            self.end_headers()


    HTTPServer(("127.0.0.1", 8080), Handler).serve_forever()
  '';
in
{
  name = "linear-webhook";

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];

    environment.systemPackages = [ send-test pkgs.curl ];

    systemd.services.fake-discord = {
      wantedBy = [ "multi-user.target" ];
      before = [ "linear-webhook.service" ];
      serviceConfig = {
        ExecStart = fake-discord;
        StateDirectory = "fake-discord";
      };
    };

    # Secret files owned by root, mode 0400, outside the Nix store.
    systemd.tmpfiles.settings."10-linear-webhook-test" = {
      "/etc/lw-secrets".d = { mode = "0700"; };
      "/etc/lw-secrets/filed-discord".f = { mode = "0400"; argument = webhook "123456789012345678" token1; };
      "/etc/lw-secrets/filed-linear".f = { mode = "0400"; argument = "filed-secret"; };
    };

    services.linear-webhook = {
      enable = true;
      listenHost = "127.0.0.1";
      listenPort = 9999;
      extraArgs = [ "--discord-proxy" "http://127.0.0.1:8080" ];
      servers = {
        filed = {
          discordWebhookUrlFile = "/etc/lw-secrets/filed-discord";
          linearSecretFile = "/etc/lw-secrets/filed-linear";
        };
        inline = {
          discordWebhookUrl = webhook "223456789012345678" token2;
          linearSecret = "inline-secret";
        };
      };
    };
    systemd.services.linear-webhook.after = [ "systemd-tmpfiles-setup.service" "fake-discord.service" ];
  };

  testScript = ''
    machine.wait_for_unit("fake-discord.service")
    machine.wait_for_unit("linear-webhook.service")
    machine.wait_for_open_port(9999)
    machine.wait_for_open_port(8080)

    def send(server, fixture, secret=""):
        return machine.succeed(
            f"send-test http://127.0.0.1:9999/{server} ${fixtures}/{fixture} {secret}"
        ).strip().splitlines()[-1]

    with subtest("signed deliveries are forwarded"):
        assert send("filed", "issue-update.json", "filed-secret") == "200"
        assert send("inline", "comment-create.json", "inline-secret") == "200"
        assert machine.succeed("ls /var/lib/fake-discord | wc -l").strip() == "2"
        machine.succeed("grep -l 'webhooks/123456789012345678/${token1}' /var/lib/fake-discord/*")
        machine.succeed("grep -l 'webhooks/223456789012345678/${token2}' /var/lib/fake-discord/*")
        machine.succeed("grep -l 'Issue updated: ENG-42' /var/lib/fake-discord/*")
        machine.succeed("grep -l 'json.zst' /var/lib/fake-discord/*")

    with subtest("unauthenticated deliveries are rejected"):
        assert send("filed", "issue-update.json") == "401"
        assert send("filed", "issue-update.json", "inline-secret") == "401"
        assert send("nope", "issue-update.json", "filed-secret") == "404"
        assert machine.succeed("ls /var/lib/fake-discord | wc -l").strip() == "2"

    with subtest("secrets stay out of the store and the journal"):
        cfg = machine.succeed(
            "systemctl show -P ExecStart linear-webhook | grep -o '/nix/store/[^ ;]*-linear-webhook.toml' | head -1"
        ).strip()
        print(machine.succeed(f"cat {cfg}"))
        machine.fail(f"grep -E '${token1}|filed-secret' {cfg}")
        machine.succeed(
            f"grep -qF 'linear_secret_file = \"/run/credentials/linear-webhook.service/filed-linear-secret\"' {cfg}"
        )
        machine.fail("journalctl -u linear-webhook | grep -E '${token1}|${token2}'")

    with subtest("Discord failure returns 502"):
        machine.succeed("systemctl stop fake-discord")
        assert send("filed", "issue-update.json", "filed-secret") == "502"
  '';
}
