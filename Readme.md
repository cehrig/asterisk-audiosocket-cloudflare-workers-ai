# Asterisk Audiosocket server to Cloudflare Workers AI

Fun AI telephone agent that responds to instructions over telephone.

The Workers AI part can be found in the `cloudflare-worker` folder. It performs
- Speech-To-Text
- Generating responses
- Text-To-Speech

The Asterisk Audiosocket server performing Voice Activity Detection (VAD), sending & receiving of audio is implemented 
in the `audiosocket-server` directory. To run the server on port 3454, sending audio to the worker running at 
https://example.com:

```shell
cargo run 3454 https://example.com
```

(requires ffmpeg to be installed)

[Read more](https://cehrig.dev/braindumps/2025-03-30-ai-telephone-agent.html)