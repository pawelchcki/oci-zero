import {
  ArtifactComment, ArtifactRetention, ArtifactSpec, ArtifactStatus,
  artifacts, configuration, event, workflow,
} from "runnerless/v1";

const comparison = new ArtifactSpec("public-artifacts",
  new ArtifactStatus("Complexity comparison", 130678843, "https://pawel.buildbuddy.io"), "comparison-files")
  .file("comment.md", "text/plain", 24576)
  .file("publication.json", "application/json", 4096)
  .file("report.json", "application/json", 16777216)
  .file("report.md", "text/plain", 16777216)
  .file("baseline.md", "text/plain", 1048576)
  .file("baseline.json", "application/json", 16777216);
comparison.retention = new ArtifactRetention(14, 5);
comparison.comment = ArtifactComment.generated("comment.md", "publication.json", 24576);

const program = workflow().publishArtifacts("complexity", comparison);

export function configure(): void {
  program.configure();
  configuration.workflow("artifact-cleanup", ["pull_request"]);
}

export function run(): void {
  program.run();
  if (event.isMerge()) artifacts.cleanup();
}
