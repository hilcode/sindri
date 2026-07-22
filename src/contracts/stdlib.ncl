let RelativeDirectory = std.contract.from_validator (fun value =>
  if !std.is_string value then
    'Error { message = "must be a relative directory (must be a string)" }
  else if std.string.is_match "^/" value then
    'Error { message = "must be a relative directory (must not start with `/`)" }
  else
    'Ok
) in
