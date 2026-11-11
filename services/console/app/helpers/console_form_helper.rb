# View helpers for rendering per-field validation errors on the legacy console
# forms. These render behavior (error lookup + markup), not static class strings;
# the field styling lives in Tailwind component classes.
module ConsoleFormHelper
  # A small red message listing the record's errors for one attribute, or nil
  # when there are none (or the record is absent, as on a fresh form).
  def field_error(record, attr)
    return if record.nil?
    messages = record.errors[attr]
    return if messages.blank?
    tag.p(messages.to_sentence, class: "field-error")
  end

  # The error modifier class to append to a field's class list when its attribute
  # is invalid, or "" otherwise.
  def field_error_class(record, attr)
    return "" if record.nil?
    record.errors[attr].present? ? "form-input-error" : ""
  end
end
