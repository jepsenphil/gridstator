# Example: Docker build

A multi-stage Dockerfile with named stages (`build`, `test`, `runner`), a build argument and a
build secret.

```bash
cd examples/docker-build
gridstator build --target runner -t gridstator-example:dev --build-arg APP_VERSION=1.2.3
gridstator build -b app                      # the same, from gridstator.yml
gridstator build -b export                   # export the image as an artifact
gridstator artifacts <job-id> --download image.tar && docker load -i image.tar
NPM_TOKEN=xyz gridstator build --target build --secret-env NPM_TOKEN
```

The image is built by BuildKit on the selected worker; its build cache stays there, so the next
build of this project on that worker is fast (the scheduler prefers it).
