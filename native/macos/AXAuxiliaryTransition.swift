import Foundation

/// Facts for a possible auxiliary-window identity transition. This is a pure
/// policy model; the live AXSession path does not call it yet.
struct AXAuxiliaryTransitionGates {
    var sameWatch: Bool
    var sameObserverGeneration: Bool
    var observerRevisionStable: Bool
    var onlyEligibleEventsSincePreviousRead: Bool
    var processStable: Bool
    var geometryStable: Bool
    var frontmostStable: Bool
    var contentStable: Bool
    var completeRead: Bool
    var observerHealthy: Bool
    var noModalOrTransient: Bool
    var activeLayerStable: Bool
    var selectedSubtreeStable: Bool
    var unaffectedSubtreesStable: Bool
    var affectedBranchesComplete: Bool
    var allRetainedEntriesClassified: Bool
}

enum AXAuxiliaryTransitionEvent {
    case destroyed(element: MenuRetainedToken, registryMatchCount: Int,
                   ownerRoot: MenuRetainedToken?, isWindowRoot: Bool)
    case windowCreated(callbackElement: MenuRetainedToken)
    case other
}

struct AXAuxiliaryTransitionInput {
    let previousRoots: [MenuRetainedToken]
    let currentRoots: [MenuRetainedToken]
    let selectedBefore: MenuRetainedToken?
    let selectedAfter: MenuRetainedToken?
    let events: [AXAuxiliaryTransitionEvent]
    let gates: AXAuxiliaryTransitionGates
}

struct AXAuxiliaryTransitionPlan {
    /// Old owner branches whose retained entries must all be retired.
    let retiredRoots: [MenuRetainedToken]
    /// Newly observed AXWindow roots whose entries must be built fresh.
    let createdRoots: [MenuRetainedToken]
}

enum AXAuxiliaryTransitionRejection: Equatable {
    case noEvents
    case eventOverflow
    case unsupportedEvent
    case watchChanged
    case observerGenerationChanged
    case observerChangedDuringRead
    case unrelatedInvalidation
    case processChanged
    case geometryChanged
    case frontmostChanged
    case contentChanged
    case incompleteRead
    case observerUnavailable
    case modalOrTransient
    case activeLayerChanged
    case selectedRootUnknown
    case selectedRootAmbiguous
    case selectedRootChanged
    case duplicatePreviousRoot
    case duplicateCurrentRoot
    case destroyedEntryUnknown
    case destroyedOwnerUnknown
    case destroyedOwnerNotPreviousRoot
    case destroyedRootMismatch
    case destroyedSelectedRoot
    case createdRootUnmatched
    case createdRootAmbiguous
    case createdSelectedRoot
    case createdRootAlreadyExisted
    case duplicateCreatedRoot
    case unexplainedRemovedRoot
    case unexplainedAddedRoot
    case selectedSubtreeChanged
    case unaffectedSubtreeChanged
    case affectedBranchIncomplete
    case unclassifiedRetainedEntry
}

enum AXAuxiliaryTransitionDecision {
    case accepted(AXAuxiliaryTransitionPlan)
    case rejected(AXAuxiliaryTransitionRejection)
}

private func cfMatchCount(_ token: MenuRetainedToken, in roots: [MenuRetainedToken]) -> Int {
    roots.reduce(into: 0) { count, root in
        if root.matches(token.element) { count += 1 }
    }
}

private func containsCFEqual(_ token: MenuRetainedToken, in roots: [MenuRetainedToken]) -> Bool {
    cfMatchCount(token, in: roots) > 0
}

private func appendIfAbsent(_ token: MenuRetainedToken, to roots: inout [MenuRetainedToken]) {
    if !containsCFEqual(token, in: roots) { roots.append(token) }
}

/// Accepts only a fully explained owner-root transition. It never uses the
/// public window string, labels, values, frames, or pointer addresses as identity.
func evaluateAXAuxiliaryTransition(_ input: AXAuxiliaryTransitionInput)
    -> AXAuxiliaryTransitionDecision {
    func reject(_ reason: AXAuxiliaryTransitionRejection) -> AXAuxiliaryTransitionDecision {
        .rejected(reason)
    }

    guard !input.events.isEmpty else { return reject(.noEvents) }
    guard input.events.count <= 8 else { return reject(.eventOverflow) }

    let gates = input.gates
    guard gates.sameWatch else { return reject(.watchChanged) }
    guard gates.sameObserverGeneration else { return reject(.observerGenerationChanged) }
    guard gates.observerRevisionStable else { return reject(.observerChangedDuringRead) }
    guard gates.onlyEligibleEventsSincePreviousRead else { return reject(.unrelatedInvalidation) }
    guard gates.processStable else { return reject(.processChanged) }
    guard gates.geometryStable else { return reject(.geometryChanged) }
    guard gates.frontmostStable else { return reject(.frontmostChanged) }
    guard gates.contentStable else { return reject(.contentChanged) }
    guard gates.completeRead else { return reject(.incompleteRead) }
    guard gates.observerHealthy else { return reject(.observerUnavailable) }
    guard gates.noModalOrTransient else { return reject(.modalOrTransient) }
    guard gates.activeLayerStable else { return reject(.activeLayerChanged) }
    guard gates.selectedSubtreeStable else { return reject(.selectedSubtreeChanged) }
    guard gates.unaffectedSubtreesStable else { return reject(.unaffectedSubtreeChanged) }
    guard gates.affectedBranchesComplete else { return reject(.affectedBranchIncomplete) }
    guard gates.allRetainedEntriesClassified else { return reject(.unclassifiedRetainedEntry) }

    for index in input.previousRoots.indices {
        guard cfMatchCount(input.previousRoots[index], in: Array(input.previousRoots.dropFirst(index + 1))) == 0 else {
            return reject(.duplicatePreviousRoot)
        }
    }
    for index in input.currentRoots.indices {
        guard cfMatchCount(input.currentRoots[index], in: Array(input.currentRoots.dropFirst(index + 1))) == 0 else {
            return reject(.duplicateCurrentRoot)
        }
    }

    guard let selectedBefore = input.selectedBefore,
          let selectedAfter = input.selectedAfter else { return reject(.selectedRootUnknown) }
    guard cfMatchCount(selectedBefore, in: input.previousRoots) == 1,
          cfMatchCount(selectedAfter, in: input.currentRoots) == 1 else {
        return reject(.selectedRootAmbiguous)
    }
    guard selectedBefore.matches(selectedAfter.element) else { return reject(.selectedRootChanged) }

    var retiredRoots: [MenuRetainedToken] = []
    var createdRoots: [MenuRetainedToken] = []
    for event in input.events {
        switch event {
        case let .destroyed(element, registryMatchCount, ownerRoot, isWindowRoot):
            guard registryMatchCount == 1 else { return reject(.destroyedEntryUnknown) }
            guard let ownerRoot else { return reject(.destroyedOwnerUnknown) }
            guard cfMatchCount(ownerRoot, in: input.previousRoots) == 1 else {
                return reject(.destroyedOwnerNotPreviousRoot)
            }
            if isWindowRoot, !element.matches(ownerRoot.element) {
                return reject(.destroyedRootMismatch)
            }
            guard !ownerRoot.matches(selectedBefore.element) else { return reject(.destroyedSelectedRoot) }
            appendIfAbsent(ownerRoot, to: &retiredRoots)

        case let .windowCreated(callbackElement):
            let matchCount = cfMatchCount(callbackElement, in: input.currentRoots)
            guard matchCount != 0 else { return reject(.createdRootUnmatched) }
            guard matchCount == 1 else { return reject(.createdRootAmbiguous) }
            guard !callbackElement.matches(selectedAfter.element) else { return reject(.createdSelectedRoot) }
            guard cfMatchCount(callbackElement, in: input.previousRoots) == 0 else {
                return reject(.createdRootAlreadyExisted)
            }
            guard !containsCFEqual(callbackElement, in: createdRoots) else {
                return reject(.duplicateCreatedRoot)
            }
            guard let root = input.currentRoots.first(where: { $0.matches(callbackElement.element) }) else {
                return reject(.createdRootUnmatched)
            }
            createdRoots.append(root)

        case .other:
            return reject(.unsupportedEvent)
        }
    }

    for previousRoot in input.previousRoots where !containsCFEqual(previousRoot, in: input.currentRoots) {
        guard containsCFEqual(previousRoot, in: retiredRoots) else { return reject(.unexplainedRemovedRoot) }
    }
    for currentRoot in input.currentRoots where !containsCFEqual(currentRoot, in: input.previousRoots) {
        guard containsCFEqual(currentRoot, in: createdRoots) else { return reject(.unexplainedAddedRoot) }
    }

    return .accepted(AXAuxiliaryTransitionPlan(retiredRoots: retiredRoots, createdRoots: createdRoots))
}
