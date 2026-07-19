# dllm-cli

CLI client for the DLLM distributed inference network.

## Binary

Installed as `dllm`.

## Commands

- **Network lifecycle**: `init`, `init-transport`, `create`, `status`, `invite`, `join`, `revoke`, `bind-transport`, `revoke-transport`
- **Onboarding**: `onboard`, `onboarding-status`
- **Forwarding**: `set-forwarder`, `remove-forwarder`
- **Model placement**: `assign`, `unassign`, `preview`, `publish-profile`
- **Credentials and policy**: `credentials`, `inference-policy`, `create-credential`, `revoke-credential`
- **Membership and access control**: `request-access`, `list-access-requests`, `approve-access`, `deny-access`, `set-budget`, `remove-budget`, `ban-node`, `unban-node`, `report-abuse`, `list-abuse-reports`, `audit-log`
- **Lifecycle and recovery**: `drain`, `resume`, `backup`, `restore`, `transfer-owner`
- **Diagnostics**: `peer-status`
