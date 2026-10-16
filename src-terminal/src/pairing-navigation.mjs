export function sectionAfterPairingInvitation(currentSection, revision, pendingProfileId) {
  if (revision > 0 && pendingProfileId) return "Connect";
  return currentSection;
}
