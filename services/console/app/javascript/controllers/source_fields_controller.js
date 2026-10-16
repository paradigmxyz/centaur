import { Controller } from "@hotwired/stimulus"

// Tailors the secret-source fields to the chosen backend: a control_plane source
// reveals an inline-value textarea, every other backend reveals a single
// reference input whose label names the backend's primary config key, the AWS
// backends additionally reveal a region input, and backends listed in the
// mountTypes value (rendered from SecretSource::MOUNT_SOURCE_TYPES) reveal a
// mount input.
export default class extends Controller {
  static targets = ["select", "reference", "referenceLabel", "inline", "region", "mount"]

  static values = { mountTypes: Array }

  static LABELS = {
    env: "Environment variable",
    aws_sm: "Secret ID",
    aws_ssm: "Parameter name",
    vault_kv: "Secret path",
    "1password": "Secret reference",
    "1password_connect": "Secret reference",
    token_broker: "Credential ID"
  }

  connect() {
    this.update()
  }

  update() {
    const type = this.selectTarget.value
    const inline = type === "control_plane"

    this.inlineTarget.hidden = !inline
    this.referenceTarget.hidden = inline
    if (this.hasRegionTarget) this.regionTarget.hidden = !(type === "aws_sm" || type === "aws_ssm")
    if (this.hasMountTarget) this.mountTarget.hidden = !this.mountTypesValue.includes(type)
    if (this.hasReferenceLabelTarget) {
      this.referenceLabelTarget.textContent = this.constructor.LABELS[type] || "Reference"
    }
  }
}
