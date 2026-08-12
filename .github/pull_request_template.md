## Summary

Describe the user-visible change and why it is needed.

## Verification

- [ ] `npm run build` in `app/`
- [ ] `cargo test --locked` in `app/src-tauri/`
- [ ] `python3 -m compileall -q bridge.py config core engines`

## Safety and Licensing

- [ ] No credentials, private keys, model weights, generated audio, or voice samples are included.
- [ ] New models or datasets include their upstream source and license information.
- [ ] Voice-related changes preserve consent and local-processing expectations.
