// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.content.Context
import helium314.keyboard.keyboard.Key
import helium314.keyboard.keyboard.Keyboard
import helium314.keyboard.keyboard.KeyboardId
import helium314.keyboard.keyboard.KeyboardSwitcher
import helium314.keyboard.keyboard.internal.keyboard_parser.floris.KeyCode
import helium314.keyboard.latin.WordComposer
import helium314.keyboard.latin.common.Constants
import org.json.JSONArray
import org.json.JSONObject

object ResearchKeyboardLayoutSnapshot {
    fun currentStateFields(context: Context): Map<String, Any?> {
        val switcher = KeyboardSwitcher.getInstance()
        val keyboardView = switcher.mainKeyboardView
        val keyboard = switcher.keyboard
        val reason = if (keyboardView == null) {
            "main_keyboard_view_unavailable"
        } else {
            "keyboard_unavailable"
        }
        return stateFields(
            keyboard = keyboard,
            unavailableReason = reason,
            shiftMode = if (keyboard != null) switcher.keyboardShiftMode else null,
            script = if (keyboard != null) switcher.currentKeyboardScript else null
        )
    }

    fun stateFieldsForKeyboard(keyboard: Keyboard?): Map<String, Any?> =
        stateFields(
            keyboard = keyboard,
            unavailableReason = "keyboard_unavailable",
            shiftMode = keyboard?.mId?.getKeyboardCapsMode(),
            script = null
        )

    fun currentLayoutJson(context: Context): JSONObject {
        val switcher = KeyboardSwitcher.getInstance()
        val keyboardView = switcher.mainKeyboardView
        val keyboard = switcher.keyboard
        val packageName = context.applicationContext.packageName
        val coordinateFrame = ResearchCoordinateFrameSnapshot.fromView(context, keyboardView)

        if (keyboardView == null) {
            return putCoordinateFrame(
                unavailable(packageName, "main_keyboard_view_unavailable"),
                coordinateFrame
            )
        }
        if (keyboard == null) {
            return putCoordinateFrame(
                unavailable(packageName, "keyboard_unavailable")
                    .put("keyboard_view_visible", keyboardView.isShown),
                coordinateFrame
            )
        }

        val locationOnScreen = IntArray(2)
        keyboardView.getLocationOnScreen(locationOnScreen)
        val visible = keyboardView.isShown
        if (!visible) {
            return putCoordinateFrame(
                unavailable(packageName, "keyboard_view_not_shown")
                    .put("keyboard_view_visible", false)
                    .put("keyboard_view_width_px", keyboardView.width)
                    .put("keyboard_view_height_px", keyboardView.height)
                    .put("keyboard_view_location_on_screen_x_px", locationOnScreen[0])
                    .put("keyboard_view_location_on_screen_y_px", locationOnScreen[1])
                    .put("keyboard_id", keyboard.mId.toString()),
                coordinateFrame
            )
        }
        val keys = keysJson(keyboard, locationOnScreen)

        return putCoordinateFrame(
            JSONObject()
            .put("available", true)
            .put("unavailable_reason", JSONObject.NULL)
            .put("package_name", packageName)
            .put("coordinate_space", "screen_px_and_keyboard_view_local_px")
            .put("tap_coordinate_fields", JSONArray().put("tap_center_screen_x_px").put("tap_center_screen_y_px"))
            .put("keyboard_view_visible", visible)
            .put("keyboard_view_width_px", keyboardView.width)
            .put("keyboard_view_height_px", keyboardView.height)
            .put("keyboard_view_location_on_screen_x_px", locationOnScreen[0])
            .put("keyboard_view_location_on_screen_y_px", locationOnScreen[1])
            .put("keyboard_id", keyboard.mId.toString())
            .put("keyboard_mode", keyboard.mId.mMode)
            .put("keyboard_mode_name", KeyboardId.modeName(keyboard.mId.mMode))
            .put("keyboard_element_id", keyboard.mId.mElementId)
            .put("keyboard_element_name", KeyboardId.elementIdToName(keyboard.mId.mElementId))
            .put("keyboard_shift_mode", switcher.keyboardShiftMode)
            .put("keyboard_shift_mode_name", capsModeName(switcher.keyboardShiftMode))
            .put("keyboard_subtype_locale", keyboard.mId.mSubtype.locale.toString())
            .put("keyboard_subtype_locale_tag", keyboard.mId.mSubtype.locale.toLanguageTag())
            .put("keyboard_subtype_main_layout_name", keyboard.mId.mSubtype.mainLayoutName)
            .put("keyboard_script", switcher.currentKeyboardScript)
            .put("keyboard_id_width_px", keyboard.mId.mWidth)
            .put("keyboard_id_height_px", keyboard.mId.mHeight)
            .put("keyboard_occupied_width_px", keyboard.mOccupiedWidth)
            .put("keyboard_occupied_height_px", keyboard.mOccupiedHeight)
            .put("keyboard_base_width_px", keyboard.mBaseWidth)
            .put("keyboard_base_height_px", keyboard.mBaseHeight)
            .put("keyboard_top_padding_px", keyboard.mTopPadding)
            .put("keyboard_vertical_gap_px", keyboard.mVerticalGap)
            .put("most_common_key_width_px", keyboard.mMostCommonKeyWidth)
            .put("most_common_key_height_px", keyboard.mMostCommonKeyHeight)
            .put("key_count", keys.length())
            .put("keys", keys),
            coordinateFrame
        )
    }

    private fun unavailable(packageName: String, reason: String): JSONObject =
        JSONObject()
            .put("available", false)
            .put("unavailable_reason", reason)
            .put("package_name", packageName)
            .put("coordinate_space", "screen_px_and_keyboard_view_local_px")
            .put("key_count", 0)
            .put("keys", JSONArray())

    private fun stateFields(
        keyboard: Keyboard?,
        unavailableReason: String,
        shiftMode: Int?,
        script: String?
    ): Map<String, Any?> {
        if (keyboard == null) return unavailableStateFields(unavailableReason)

        val id = keyboard.mId
        val subtype = id.mSubtype
        val effectiveShiftMode = shiftMode ?: id.getKeyboardCapsMode()
        return mapOf(
            "keyboard_state_available" to true,
            "keyboard_state_unavailable_reason" to null,
            "keyboard_id" to id.toString(),
            "keyboard_mode" to id.mMode,
            "keyboard_mode_name" to KeyboardId.modeName(id.mMode),
            "keyboard_element_id" to id.mElementId,
            "keyboard_element_name" to KeyboardId.elementIdToName(id.mElementId),
            "keyboard_shift_mode" to effectiveShiftMode,
            "keyboard_shift_mode_name" to capsModeName(effectiveShiftMode),
            "keyboard_shifted" to (effectiveShiftMode != WordComposer.CAPS_MODE_OFF),
            "keyboard_shift_source" to capsModeSource(effectiveShiftMode),
            "keyboard_caps_locked" to isCapsLocked(effectiveShiftMode),
            "keyboard_is_alphabet" to id.isAlphabetKeyboard(),
            "keyboard_is_alpha_or_symbol" to id.isAlphaOrSymbolKeyboard(),
            "keyboard_is_alphabet_shifted" to id.isAlphabetShifted(),
            "keyboard_is_alphabet_shifted_manually" to id.isAlphabetShiftedManually(),
            "keyboard_is_number_layout" to id.isNumberLayout(),
            "keyboard_is_emoji" to id.isEmojiKeyboard(),
            "keyboard_subtype_locale" to subtype.locale.toString(),
            "keyboard_subtype_locale_tag" to subtype.locale.toLanguageTag(),
            "keyboard_subtype_main_layout_name" to subtype.mainLayoutName,
            "keyboard_subtype_is_rtl" to subtype.isRtlSubtype,
            "keyboard_subtype_is_no_language" to subtype.isNoLanguage,
            "keyboard_script" to script,
            "keyboard_id_width_px" to id.mWidth,
            "keyboard_id_height_px" to id.mHeight
        )
    }

    private fun unavailableStateFields(reason: String): Map<String, Any?> =
        mapOf(
            "keyboard_state_available" to false,
            "keyboard_state_unavailable_reason" to reason,
            "keyboard_id" to null,
            "keyboard_mode" to null,
            "keyboard_mode_name" to null,
            "keyboard_element_id" to null,
            "keyboard_element_name" to null,
            "keyboard_shift_mode" to null,
            "keyboard_shift_mode_name" to null,
            "keyboard_shifted" to null,
            "keyboard_shift_source" to null,
            "keyboard_caps_locked" to null,
            "keyboard_is_alphabet" to null,
            "keyboard_is_alpha_or_symbol" to null,
            "keyboard_is_alphabet_shifted" to null,
            "keyboard_is_alphabet_shifted_manually" to null,
            "keyboard_is_number_layout" to null,
            "keyboard_is_emoji" to null,
            "keyboard_subtype_locale" to null,
            "keyboard_subtype_locale_tag" to null,
            "keyboard_subtype_main_layout_name" to null,
            "keyboard_subtype_is_rtl" to null,
            "keyboard_subtype_is_no_language" to null,
            "keyboard_script" to null,
            "keyboard_id_width_px" to null,
            "keyboard_id_height_px" to null
        )

    private fun capsModeName(mode: Int?): String? =
        when (mode) {
            WordComposer.CAPS_MODE_OFF -> "off"
            WordComposer.CAPS_MODE_MANUAL_SHIFTED -> "manual_shifted"
            WordComposer.CAPS_MODE_MANUAL_SHIFT_LOCKED -> "manual_shift_locked"
            WordComposer.CAPS_MODE_AUTO_SHIFTED -> "auto_shifted"
            WordComposer.CAPS_MODE_AUTO_SHIFT_LOCKED -> "auto_shift_locked"
            else -> null
        }

    private fun capsModeSource(mode: Int?): String? =
        when (mode) {
            WordComposer.CAPS_MODE_OFF -> "off"
            WordComposer.CAPS_MODE_MANUAL_SHIFTED,
            WordComposer.CAPS_MODE_MANUAL_SHIFT_LOCKED -> "manual"
            WordComposer.CAPS_MODE_AUTO_SHIFTED,
            WordComposer.CAPS_MODE_AUTO_SHIFT_LOCKED -> "auto"
            else -> null
        }

    private fun isCapsLocked(mode: Int?): Boolean? =
        when (mode) {
            WordComposer.CAPS_MODE_OFF,
            WordComposer.CAPS_MODE_MANUAL_SHIFTED,
            WordComposer.CAPS_MODE_AUTO_SHIFTED -> false
            WordComposer.CAPS_MODE_MANUAL_SHIFT_LOCKED,
            WordComposer.CAPS_MODE_AUTO_SHIFT_LOCKED -> true
            else -> null
        }

    private fun putCoordinateFrame(
        json: JSONObject,
        coordinateFrame: ResearchCoordinateFrameSnapshot.CoordinateFrameSnapshot
    ): JSONObject {
        coordinateFrame.fields().forEach { (key, value) ->
            if (!json.has(key)) {
                json.put(key, jsonValue(value))
            }
        }
        return json
    }

    private fun keysJson(keyboard: Keyboard, locationOnScreen: IntArray): JSONArray {
        val keys = JSONArray()
        keyboard.sortedKeys.forEachIndexed { index, key ->
            if (!key.isSpacer) {
                keys.put(keyJson(index, key, locationOnScreen))
            }
        }
        return keys
    }

    private fun keyJson(index: Int, key: Key, locationOnScreen: IntArray): JSONObject {
        val hitBox = key.hitBox
        val keyCenterLocalX = key.x + key.width / 2.0
        val keyCenterLocalY = key.y + key.height / 2.0
        val hitBoxCenterLocalX = (hitBox.left + hitBox.right) / 2.0
        val hitBoxCenterLocalY = (hitBox.top + hitBox.bottom) / 2.0
        val tapCenterScreenX = locationOnScreen[0] + keyCenterLocalX
        val tapCenterScreenY = locationOnScreen[1] + keyCenterLocalY

        return JSONObject()
            .put("index", index)
            .put("key_code", key.code)
            .put("key_code_printable", Constants.printableCode(key.code))
            .put("key_label", jsonValue(key.label))
            .put("key_hint_label", jsonValue(key.hintLabel))
            .put("key_preview_label", jsonValue(key.previewLabel))
            .put("key_output_text", jsonValue(key.outputText))
            .put("key_icon_name", jsonValue(key.iconName))
            .put("key_alt_code", key.altCode)
            .put("key_class", keyClass(key))
            .put("key_background", keyBackground(key.backgroundType))
            .put("key_background_type", key.backgroundType)
            .put("key_short_string", key.toShortString())
            .put("key_long_string", key.toLongString())
            .put("key_x_px", key.x)
            .put("key_y_px", key.y)
            .put("key_width_px", key.width)
            .put("key_height_px", key.height)
            .put("key_draw_x_px", key.drawX)
            .put("key_draw_width_px", key.drawWidth)
            .put("key_horizontal_gap_px", key.horizontalGap)
            .put("key_vertical_gap_px", key.verticalGap)
            .put("key_hitbox_left_px", hitBox.left)
            .put("key_hitbox_top_px", hitBox.top)
            .put("key_hitbox_right_px", hitBox.right)
            .put("key_hitbox_bottom_px", hitBox.bottom)
            .put("key_center_local_x_px", keyCenterLocalX)
            .put("key_center_local_y_px", keyCenterLocalY)
            .put("hitbox_center_local_x_px", hitBoxCenterLocalX)
            .put("hitbox_center_local_y_px", hitBoxCenterLocalY)
            .put("tap_center_screen_x_px", tapCenterScreenX)
            .put("tap_center_screen_y_px", tapCenterScreenY)
            .put("hitbox_left_screen_px", locationOnScreen[0] + hitBox.left)
            .put("hitbox_top_screen_px", locationOnScreen[1] + hitBox.top)
            .put("hitbox_right_screen_px", locationOnScreen[0] + hitBox.right)
            .put("hitbox_bottom_screen_px", locationOnScreen[1] + hitBox.bottom)
            .put("enabled", key.isEnabled)
            .put("modifier", key.isModifier)
            .put("shift", key.isShift)
            .put("repeatable", key.isRepeatable)
            .put("preview_enabled", key.hasPreview())
            .put("long_press_enabled", key.isLongPressEnabled)
            .put("alt_code_while_typing", key.altCodeWhileTyping())
            .put("has_action_background", key.hasActionKeyBackground())
            .put("has_functional_background", key.hasFunctionalBackground())
            .put("has_popup_hint", key.hasPopupHint())
            .put("popup_count", key.popupKeys?.size ?: 0)
    }

    private fun keyClass(key: Key): String {
        val code = key.code
        return when {
            code == Constants.CODE_SPACE || code == KeyCode.CJK_SPACE || code == KeyCode.ZWNJ -> "space"
            code == Constants.CODE_ENTER || code == KeyCode.SHIFT_ENTER -> "enter"
            code == Constants.CODE_TAB || code == KeyCode.TAB -> "tab"
            code == KeyCode.DELETE -> "delete"
            code == KeyCode.LANGUAGE_SWITCH -> "language_switch"
            code == KeyCode.EMOJI || code == KeyCode.EMOJI_SEARCH -> "emoji"
            code == KeyCode.CLIPBOARD ||
                    code == KeyCode.CLIPBOARD_COPY ||
                    code == KeyCode.CLIPBOARD_CUT ||
                    code == KeyCode.CLIPBOARD_PASTE ||
                    code == KeyCode.CLIPBOARD_SELECT_WORD ||
                    code == KeyCode.CLIPBOARD_SELECT_ALL ||
                    code == KeyCode.CLIPBOARD_CLEAR_HISTORY ||
                    code == KeyCode.CLIPBOARD_COPY_ALL -> "clipboard"
            key.isModifier -> "modifier"
            key.hasActionKeyBackground() -> "action"
            code < 0 -> "function"
            Character.isLetter(code) -> "letter"
            Character.isDigit(code) -> "digit"
            Character.isWhitespace(code) -> "whitespace"
            code >= Constants.CODE_SPACE -> "symbol"
            else -> "unknown"
        }
    }

    private fun keyBackground(backgroundType: Int): String =
        when (backgroundType) {
            Key.BACKGROUND_TYPE_EMPTY -> "empty"
            Key.BACKGROUND_TYPE_NORMAL -> "normal"
            Key.BACKGROUND_TYPE_FUNCTIONAL -> "functional"
            Key.BACKGROUND_TYPE_ACTION -> "action"
            Key.BACKGROUND_TYPE_SPACEBAR -> "spacebar"
            else -> "unknown"
        }

    private fun jsonValue(value: Any?): Any =
        when (value) {
            null -> JSONObject.NULL
            is Boolean, is Number, is String, is JSONObject, is JSONArray -> value
            else -> value.toString()
        }
}
