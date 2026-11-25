# Remote SSH

```bash
remote-ssh exec --url wss://dev.example.com/events/events -- command arg
remote-ssh read --url wss://dev.example.com/events/events path/to/file
remote-ssh write --url wss://dev.example.com/events/events path/to/file < local-file
```

Runs in the configured workspace through the paired proxy using a disposable key.
Commands are not retried automatically. See `docs/pages/operate/ssh.mdx` for setup.
