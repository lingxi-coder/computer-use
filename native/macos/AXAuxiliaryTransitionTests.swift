import Foundation

/// Pure CFEqual-based checks for the auxiliary-root transition model. These do
/// not call AX, change the live registry policy, or grant input authority.
func runAuxiliaryTransitionTests() throws -> [String] {
    func token(_ value: String) -> MenuRetainedToken { MenuRetainedToken(value as CFString) }

    func gates() -> AXAuxiliaryTransitionGates {
        AXAuxiliaryTransitionGates(
            sameWatch: true,
            sameObserverGeneration: true,
            observerRevisionStable: true,
            onlyEligibleEventsSincePreviousRead: true,
            processStable: true,
            geometryStable: true,
            frontmostStable: true,
            contentStable: true,
            completeRead: true,
            observerHealthy: true,
            noModalOrTransient: true,
            activeLayerStable: true,
            selectedSubtreeStable: true,
            unaffectedSubtreesStable: true,
            affectedBranchesComplete: true,
            allRetainedEntriesClassified: true)
    }

    func input(previous: [MenuRetainedToken], current: [MenuRetainedToken],
               selectedBefore: MenuRetainedToken?, selectedAfter: MenuRetainedToken?,
               events: [AXAuxiliaryTransitionEvent], gates: AXAuxiliaryTransitionGates? = nil)
        -> AXAuxiliaryTransitionInput {
        AXAuxiliaryTransitionInput(previousRoots: previous, currentRoots: current,
            selectedBefore: selectedBefore, selectedAfter: selectedAfter,
            events: events, gates: gates ?? selfGates())
    }

    // Local indirection keeps the input helper's default independent of other tests.
    func selfGates() -> AXAuxiliaryTransitionGates { gates() }

    func acceptedPlan(_ value: AXAuxiliaryTransitionInput) -> AXAuxiliaryTransitionPlan? {
        guard case let .accepted(plan) = evaluateAXAuxiliaryTransition(value) else { return nil }
        return plan
    }

    func rejection(_ value: AXAuxiliaryTransitionInput) -> AXAuxiliaryTransitionRejection? {
        guard case let .rejected(reason) = evaluateAXAuxiliaryTransition(value) else { return nil }
        return reason
    }

    let selected = token("selected-root")
    let selectedCopy = token("selected-root")
    let auxiliary = token("auxiliary-root")
    let auxiliaryCopy = token("auxiliary-root")
    let unaffected = token("unaffected-root")
    let unaffectedCopy = token("unaffected-root")

    let buttonDestroy = AXAuxiliaryTransitionEvent.destroyed(
        element: token("destroyed-button"), registryMatchCount: 1,
        ownerRoot: auxiliaryCopy, isWindowRoot: false)
    let buttonInput = input(previous: [selected, auxiliary, unaffected],
        current: [selectedCopy, auxiliaryCopy, unaffectedCopy],
        selectedBefore: selected, selectedAfter: selectedCopy, events: [buttonDestroy])
    guard let buttonPlan = acceptedPlan(buttonInput) else {
        throw NativeFailure("self-test failed: exact nonselected child destroy should select its owner branch")
    }
    try requireTest(buttonPlan.retiredRoots.count == 1
                    && buttonPlan.retiredRoots[0].matches(auxiliary.element)
                    && buttonPlan.createdRoots.isEmpty,
                    "auxiliary child destroy retires only its exact owner root")

    let replacement = token("created-auxiliary-root")
    let replacementCopy = token("created-auxiliary-root")
    let rootReplacement = input(previous: [selected, auxiliary, unaffected],
        current: [selectedCopy, replacement, unaffectedCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [
            .destroyed(element: auxiliaryCopy, registryMatchCount: 1,
                       ownerRoot: auxiliary, isWindowRoot: true),
            .windowCreated(callbackElement: replacementCopy)
        ])
    guard let replacementPlan = acceptedPlan(rootReplacement) else {
        throw NativeFailure("self-test failed: explained auxiliary root replacement should be accepted")
    }
    try requireTest(replacementPlan.retiredRoots.count == 1
                    && replacementPlan.retiredRoots[0].matches(auxiliary.element)
                    && replacementPlan.createdRoots.count == 1
                    && replacementPlan.createdRoots[0].matches(replacement.element),
                    "destroyed and created auxiliary roots form an exact branch plan")

    // Z9EKAE-shaped evidence: the destroyed old root is nonselected, but the
    // WindowCreated callback has no unique CFEqual match in the fresh AX roots.
    let z9UnmatchedCreated = input(previous: [selected, auxiliary, unaffected],
        current: [selectedCopy, replacement, unaffectedCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [
            .destroyed(element: auxiliaryCopy, registryMatchCount: 1,
                       ownerRoot: auxiliary, isWindowRoot: true),
            .windowCreated(callbackElement: token("unmatched-created-callback"))
        ])
    try requireTest(rejection(z9UnmatchedCreated) == .createdRootUnmatched,
                    "Z9EKAE destroy with zero created-root matches remains rejected")

    let createdNoMatch = input(previous: [selected], current: [selectedCopy, replacement],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.windowCreated(callbackElement: token("not-current"))])
    let createdAmbiguous = input(previous: [selected],
        current: [selectedCopy, replacement, replacementCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.windowCreated(callbackElement: replacement)])
    let createdSelected = input(previous: [selected], current: [selectedCopy, replacement],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.windowCreated(callbackElement: selectedCopy)])
    let createdOld = input(previous: [selected, auxiliary], current: [selectedCopy, auxiliaryCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.windowCreated(callbackElement: auxiliaryCopy)])
    try requireTest(rejection(createdNoMatch) == .createdRootUnmatched
                    && rejection(createdAmbiguous) == .duplicateCurrentRoot
                    && rejection(createdSelected) == .createdSelectedRoot
                    && rejection(createdOld) == .createdRootAlreadyExisted,
                    "WindowCreated needs one current, nonselected, genuinely new AX root")

    let selectedDestroy = input(previous: [selected], current: [selectedCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: selectedCopy, registryMatchCount: 1,
                            ownerRoot: selected, isWindowRoot: true)])
    let unknownDestroy = input(previous: [selected, auxiliary], current: [selectedCopy, auxiliaryCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: token("unknown"), registryMatchCount: 0,
                            ownerRoot: nil, isWindowRoot: false)])
    let missingOwnerDestroy = input(previous: [selected, auxiliary], current: [selectedCopy, auxiliaryCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: token("missing-owner"), registryMatchCount: 1,
                            ownerRoot: nil, isWindowRoot: false)])
    let duplicateDestroy = input(previous: [selected, auxiliary], current: [selectedCopy, auxiliaryCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: token("duplicate"), registryMatchCount: 2,
                            ownerRoot: auxiliary, isWindowRoot: false)])
    let wrongRootDestroy = input(previous: [selected, auxiliary], current: [selectedCopy, auxiliaryCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: token("not-the-root"), registryMatchCount: 1,
                            ownerRoot: auxiliary, isWindowRoot: true)])
    try requireTest(rejection(selectedDestroy) == .destroyedSelectedRoot
                    && rejection(unknownDestroy) == .destroyedEntryUnknown
                    && rejection(missingOwnerDestroy) == .destroyedOwnerUnknown
                    && rejection(duplicateDestroy) == .destroyedEntryUnknown
                    && rejection(wrongRootDestroy) == .destroyedRootMismatch,
                    "destroy events require one retained entry and exact nonselected owner")

    let unexplainedAdd = input(previous: [selected], current: [selectedCopy, replacement],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: token("button"), registryMatchCount: 1,
                            ownerRoot: token("missing-old-root"), isWindowRoot: false)])
    let unexplainedRemoval = input(previous: [selected, auxiliary, unaffected], current: [selectedCopy],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: token("button"), registryMatchCount: 1,
                            ownerRoot: auxiliaryCopy, isWindowRoot: false)])
    let unexplainedAddition = input(previous: [selected, auxiliary, unaffected],
        current: [selectedCopy, auxiliaryCopy, unaffectedCopy, token("unannounced-root")],
        selectedBefore: selected, selectedAfter: selectedCopy,
        events: [.destroyed(element: token("button"), registryMatchCount: 1,
                            ownerRoot: auxiliaryCopy, isWindowRoot: false)])
    try requireTest(rejection(unexplainedAdd) == .destroyedOwnerNotPreviousRoot
                    && rejection(unexplainedRemoval) == .unexplainedRemovedRoot
                    && rejection(unexplainedAddition) == .unexplainedAddedRoot,
                    "root inventory changes must be fully accounted for by exact events")

    let missingSelectedRoot = input(previous: [selected], current: [selectedCopy],
        selectedBefore: nil, selectedAfter: selectedCopy, events: [buttonDestroy])
    let ambiguousSelectedRoot = input(previous: [selected, selectedCopy],
        current: [selectedCopy], selectedBefore: selected, selectedAfter: selectedCopy,
        events: [buttonDestroy])
    let changedSelectedRoot = input(previous: [selected], current: [replacement],
        selectedBefore: selected, selectedAfter: replacement,
        events: [.windowCreated(callbackElement: replacement)])
    try requireTest(rejection(missingSelectedRoot) == .selectedRootUnknown
                    && rejection(ambiguousSelectedRoot) == .duplicatePreviousRoot
                    && rejection(changedSelectedRoot) == .selectedRootChanged,
                    "selected-root absence, ambiguity and replacement fail closed")

    var gateCases: [(String, (inout AXAuxiliaryTransitionGates) -> Void,
                     AXAuxiliaryTransitionRejection)] = []
    gateCases.append(("watch", { $0.sameWatch = false }, .watchChanged))
    gateCases.append(("observer generation", { $0.sameObserverGeneration = false }, .observerGenerationChanged))
    gateCases.append(("read notification", { $0.observerRevisionStable = false }, .observerChangedDuringRead))
    gateCases.append(("unrelated event", { $0.onlyEligibleEventsSincePreviousRead = false }, .unrelatedInvalidation))
    gateCases.append(("process", { $0.processStable = false }, .processChanged))
    gateCases.append(("geometry", { $0.geometryStable = false }, .geometryChanged))
    gateCases.append(("frontmost", { $0.frontmostStable = false }, .frontmostChanged))
    gateCases.append(("content", { $0.contentStable = false }, .contentChanged))
    gateCases.append(("partial tree", { $0.completeRead = false }, .incompleteRead))
    gateCases.append(("observer health", { $0.observerHealthy = false }, .observerUnavailable))
    gateCases.append(("modal", { $0.noModalOrTransient = false }, .modalOrTransient))
    gateCases.append(("active layer", { $0.activeLayerStable = false }, .activeLayerChanged))
    gateCases.append(("selected subtree", { $0.selectedSubtreeStable = false }, .selectedSubtreeChanged))
    gateCases.append(("unaffected subtree", { $0.unaffectedSubtreesStable = false }, .unaffectedSubtreeChanged))
    gateCases.append(("affected branch", { $0.affectedBranchesComplete = false }, .affectedBranchIncomplete))
    gateCases.append(("entry accounting", { $0.allRetainedEntriesClassified = false }, .unclassifiedRetainedEntry))
    for (name, alter, expected) in gateCases {
        var testGates = gates()
        alter(&testGates)
        let gated = input(previous: [selected, auxiliary], current: [selectedCopy, auxiliaryCopy],
            selectedBefore: selected, selectedAfter: selectedCopy, events: [buttonDestroy], gates: testGates)
        try requireTest(rejection(gated) == expected, "aux transition rejects changed \(name) gate")
    }
    try requireTest(rejection(input(previous: [selected], current: [selectedCopy],
                                    selectedBefore: selected, selectedAfter: selectedCopy,
                                    events: [.other])) == .unsupportedEvent
                    && rejection(input(previous: [selected], current: [selectedCopy],
                                       selectedBefore: selected, selectedAfter: selectedCopy,
                                       events: Array(repeating: .other, count: 9))) == .eventOverflow,
                    "unsupported notifications and bounded-event overflow fail closed")

    return [
        "aux_transition_retires_exact_nonselected_owner_branch",
        "aux_transition_requires_unique_created_ax_root",
        "aux_transition_rejects_z9ekae_unmatched_created_root",
        "aux_transition_rejects_selected_or_unresolved_destroy",
        "aux_transition_requires_same_unique_selected_root",
        "aux_transition_requires_exact_root_inventory_accounting",
        "aux_transition_gates_and_event_overflow_fail_closed"
    ]
}
