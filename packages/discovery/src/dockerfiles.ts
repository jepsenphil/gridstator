export interface DockerfileInfo {
  /** Path relative to the project root. */
  file: string;
  /** Default build context: the Dockerfile's directory. */
  context: string;
  /** Named stages (`FROM … AS name`), usable as build targets. */
  stages: string[];
  baseImages: string[];
}
