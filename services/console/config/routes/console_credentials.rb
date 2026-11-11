# Credentials slice (broker credentials). The pages live in the SPA
# (web/src/features/credentials) on top of /api/v1/broker_credentials; Rails
# only keeps path helpers for links from legacy pages.
direct(:console_credentials) { "/console/credentials" }
direct(:console_credential) { |oid| "/console/credentials/#{oid}" }
