# Probe status

The ext-image-copy capture path is **not approved for applet integration yet**.

Current status:

- implementation branch: `experiment/ext-image-copy-probe`
- capture path: foreign-toplevel image capture source + `ext_image_copy_capture_v1`
- applet integration: none
- `tihulu-previewd`: design only, blocked on real runtime probe
- `tihulu-mediad`: design only, later phase

Approval requires a real COSMIC 500-capture run with bounded client/compositor FD, memfd, and RSS behavior.
