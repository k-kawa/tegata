# Running on servers

Let servers pick up new values by themselves, so that a rotation needs no login on each server. A timer runs `tegata render` every few minutes; it writes only the files whose content changed, and restarts or reloads only what uses them.

## 1. What a server needs

- The `tegata` executable ([Install](../../README.md#install)). No configuration file, no providers, no `tegata init`.
- An identity that can read its secrets: on a Compute Engine VM, its service account, listed in an [identity](../reference/configuration.md#identities) that is a reader of those secrets. `tegata check <identity> <templates>` checks that it can read every name in its templates.
- Its templates, for example in `/opt/app/templates`.

## 2. Render on a timer

```ini
# /etc/systemd/system/tegata-render-app.service
[Unit]
Description=Fill the app's configuration from Secret Manager
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/tegata render --fail-on-missing \
  --on-change 'systemctl reload-or-restart app.service' \
  /opt/app/templates /etc/app
```

```ini
# /etc/systemd/system/tegata-render-app.timer
[Unit]
Description=Fill the app's configuration from Secret Manager every 5 minutes

[Timer]
OnBootSec=1min
OnUnitActiveSec=5min
RandomizedDelaySec=60

[Install]
WantedBy=timers.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now tegata-render-app.timer
journalctl -u tegata-render-app.service   # "changed: ..." and "ran: ..." when something changed
```

- `--on-change` runs only when a file changed, once per run, after all files are written. Nothing changed means nothing is written and nothing restarts.
- `--fail-on-missing` keeps a broken grant (a name the server cannot read) from being written to disk as `__NAME__`. The run fails, the old files stay, and the journal says which names.
- On a Compute Engine VM, the project comes from the metadata server; elsewhere add `--project <project>`, and give the server a way to authenticate to Google Cloud.
- Outside systemd (cron, a container's entrypoint), `--exit-code` tells the caller whether anything changed: 3 when it did, 0 when it did not.

### Restarting only what changed

When one template directory feeds several services, use the changed files' names in `TEGATA_CHANGED`:

```sh
#!/bin/sh
# /usr/local/bin/app-reload: called by --on-change
for f in $TEGATA_CHANGED; do
  case "$f" in
    app.env) systemctl restart app.service ;;
    cloudflared.env) systemctl restart cloudflared.service ;;
    *) ;;
  esac
done
```

## 3. Rotating with the timer

The server side of a [rotation](rotating-secrets.md) becomes waiting for one timer period (here, at most 6 minutes):

```sh
tegata rotate session_key     # the servers pick up the new value within one period
tegata settle session_key     # after the old value is no longer needed; again one period
tegata prune session_key
```

Before `prune`, make sure every server has the new value, for example by looking for the `changed:` line in each server's journal. A value replaced at once (a database password, a tunnel token) breaks new connections until the timer runs; for those, run the service by hand on each server right after `rotate` (`systemctl start tegata-render-app.service`) instead of waiting.

## 4. How often, and what it costs

Each run reads every secret in the templates once. Secret Manager charges for these reads: $0.03 per 10,000, after 10,000 free reads a month per billing account ([pricing](https://cloud.google.com/secret-manager/pricing)).

| Every | Runs a month | 10 secrets, 1 server | 10 secrets, 5 servers |
| --- | --- | --- | --- |
| 5 minutes | 8,640 | 86,400 reads, about $0.23 | 432,000 reads, about $1.27 |
| 15 minutes | 2,880 | 28,800 reads, about $0.05 | 144,000 reads, about $0.40 |
| 1 hour | 720 | 7,200 reads, free | 36,000 reads, about $0.08 |

The figures take off the 10,000 free reads once. A shorter period means rotations reach the servers sooner; a longer one costs less. Five to fifteen minutes suits most services.
