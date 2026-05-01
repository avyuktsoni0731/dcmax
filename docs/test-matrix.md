# MyCord v1 Validation Matrix

## Target Matrix

| OS | Browser | Voice Call | Screen Share | Screen Audio |
| --- | --- | --- | --- | --- |
| Windows 11 | Chrome latest | Required | Required | Expected (system audio often works) |
| Windows 11 | Edge latest | Required | Required | Expected (system audio often works) |
| macOS latest | Chrome latest | Required | Required | Partial (tab audio preferred) |
| macOS latest | Edge latest | Optional | Optional | Partial |

## Negative Tests

- Deny mic permission at join and verify graceful error state.
- Deny display capture permission and verify retry path.
- Disconnect/reconnect network and verify call state transitions.
- Hot-swap microphone while connected and verify audio continuity.

## Known Limitations

- Full system audio sharing is browser and OS dependent in pure web apps.
- Safari and Firefox are not primary v1 targets for screen-audio reliability.
- v1 optimizes for smooth 1080p60, not guaranteed 4K60.
