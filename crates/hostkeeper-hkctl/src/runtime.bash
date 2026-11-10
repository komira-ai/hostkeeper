# The runtime of hkctl: the same in every release. The generated part after it holds the version,
# the help texts and one spec per method, and calls _hk_main last, so a script cut short runs
# nothing.
#
# Rules this file keeps, which crates/hostkeeper-hkctl/tests check:
# - no construct newer than bash 3.2 (no associative array, mapfile, case conversion, ...);
# - no token or secret in a file it writes, in an argument list, in a child's environment or in a
#   diagnostic: a secret lives in an unexported shell variable and reaches curl through a pipe of
#   a process substitution, filled by the printf builtin in a subshell that execs nothing; no
#   here-document or here-string (bash may write one to a temporary file), no export;
# - the only programs it starts are curl and find;
# - the one more call a refusal `invalid_request` makes, to the coordinator's
#   `GET /v1/version?at_least=<this release>`, carries no token and no credential pipe.

set -f
LC_ALL=C

# Every variable of this script, unset before any is set: a variable of the same name in the
# caller's environment would keep its export attribute, and its value would reach curl.
unset _hk_token _hk_oidc_url _hk_oidc_request _hk_body _hk_vals _hk_value _hk_reply _hk_json \
	_hk_set _hk_secret_src _hk_mapkeys _hk_spec _hk_keys _hk_kvals _hk_inner _hk_trimmed \
	_hk_endpoint _hk_endpoint_set _hk_ca _hk_ca_set _hk_token_file _hk_token_file_set \
	_hk_token_stdin _hk_audience _hk_audience_set _hk_response _hk_response_set _hk_timeout \
	_hk_timeout_set _hk_stdin_by _hk_took _hk_arg_value _hk_request _hk_request_set _hk_path \
	_hk_command _hk_helpfn _hk_hold _hk_key_escaped _hk_f_flag _hk_f_name _hk_f_json \
	_hk_f_input _hk_f_kind _hk_f_label _hk_f_enum _hk_b64 _hk_ws _hk_re_float _hk_re_refusal \
	_hk_re_reason _hk_re_endpoint _hk_re_secs _hk_hold_text _hk_re_version
# The runner's two variables of CI's token (rule S2: secret material), kept unexported and
# unset before any child starts.
_hk_oidc_url=${ACTIONS_ID_TOKEN_REQUEST_URL-}
_hk_oidc_request=${ACTIONS_ID_TOKEN_REQUEST_TOKEN-}
unset ACTIONS_ID_TOKEN_REQUEST_URL ACTIONS_ID_TOKEN_REQUEST_TOKEN

_hk_ws=$' \t\n\r'
_hk_re_float='^-?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?$'
_hk_re_refusal='^\{"code":"([A-Z_]+)","reason":"(.*)"\}$'
_hk_re_reason='^[a-z0-9_]{1,64}$'
_hk_re_endpoint='^https://([A-Za-z0-9.-]+|\[[0-9A-Fa-f:.]+\])(:[0-9]{1,5})?/?$'
_hk_re_secs='^[1-9][0-9]{0,5}$'
_hk_re_version='"version":"([^"]*)"'

# A usage error: exit 2.
_hk_usage() {
	printf 'hkctl: %s\n' "$1" >&2
	exit 2
}

# Anything else: exit 1.
_hk_fail() {
	printf 'hkctl: %s\n' "$1" >&2
	exit 1
}

# _hk_stdin FLAG: FLAG reads stdin; a second input that reads it is a usage error.
_hk_stdin() {
	if [[ -n $_hk_stdin_by ]]; then
		_hk_usage "$_hk_stdin_by and $1 both read stdin; at most one input may"
	fi
	_hk_stdin_by=$1
}

# _hk_arg FLAG ARGS...: the value of the flag FLAG, the first of ARGS: `--f=v`, or `--f v`.
# Sets _hk_arg_value and _hk_took, the number of arguments taken.
_hk_arg() {
	local flag=$1
	shift
	case $1 in
	--*=*)
		_hk_arg_value=${1#*=}
		_hk_took=1
		;;
	*)
		if (($# < 2)); then
			_hk_usage "$flag needs a value"
		fi
		case $2 in
		--*) _hk_usage "$flag needs a value" ;;
		esac
		_hk_arg_value=$2
		_hk_took=2
		;;
	esac
}

# _hk_switch FLAG ARG: refuses a value given to the switch FLAG as `--f=v`.
_hk_switch() {
	case $2 in
	*=*) _hk_usage "$1 takes no value" ;;
	esac
	_hk_took=1
}

# _hk_global ARGS...: takes a global flag at the start of ARGS, with its value, and sets _hk_took;
# returns 1 when ARGS do not start with one.
_hk_global() {
	local flag=${1%%=*}
	case $flag in
	--token-stdin)
		_hk_switch "$flag" "$1"
		if ((_hk_token_stdin)); then
			_hk_usage "$flag is given twice"
		fi
		_hk_token_stdin=1
		_hk_stdin "$flag"
		return 0
		;;
	--endpoint | --ca-file | --token-file | --github-oidc-audience | --response-file | --timeout-secs) ;;
	*) return 1 ;;
	esac
	_hk_arg "$flag" "$@"
	case $flag in
	--endpoint)
		if ((_hk_endpoint_set)); then _hk_usage "$flag is given twice"; fi
		_hk_endpoint_set=1
		_hk_endpoint=$_hk_arg_value
		;;
	--ca-file)
		if ((_hk_ca_set)); then _hk_usage "$flag is given twice"; fi
		_hk_ca_set=1
		_hk_ca=$_hk_arg_value
		;;
	--token-file)
		if ((_hk_token_file_set)); then _hk_usage "$flag is given twice"; fi
		_hk_token_file_set=1
		_hk_token_file=$_hk_arg_value
		;;
	--github-oidc-audience)
		if ((_hk_audience_set)); then _hk_usage "$flag is given twice"; fi
		_hk_audience_set=1
		_hk_audience=$_hk_arg_value
		;;
	--response-file)
		if ((_hk_response_set)); then _hk_usage "$flag is given twice"; fi
		_hk_response_set=1
		_hk_response=$_hk_arg_value
		;;
	*)
		if ((_hk_timeout_set)); then _hk_usage "$flag is given twice"; fi
		if ! [[ $_hk_arg_value =~ $_hk_re_secs ]]; then
			_hk_usage "$flag takes a whole number of seconds, 1 to 999999"
		fi
		_hk_timeout_set=1
		_hk_timeout=$_hk_arg_value
		;;
	esac
	return 0
}

# _hk_parse_spec ENTRY: the parts of one field's spec,
# `flag|name|json|input|kind|label|spelling=NAME,...`, as _hk_f_*.
_hk_parse_spec() {
	local rest=$1
	_hk_f_flag=${rest%%|*}
	rest=${rest#*|}
	_hk_f_name=${rest%%|*}
	rest=${rest#*|}
	_hk_f_json=${rest%%|*}
	rest=${rest#*|}
	_hk_f_input=${rest%%|*}
	rest=${rest#*|}
	_hk_f_kind=${rest%%|*}
	rest=${rest#*|}
	_hk_f_label=${rest%%|*}
	_hk_f_enum=${rest#*|}
}

# _hk_json_string TEXT: TEXT as a JSON string, in _hk_json.
_hk_json_string() {
	local s=$1 out='' c n i=0 len
	case $s in
	*[!\ -~]* | *'"'* | *'\'*) ;;
	*)
		_hk_json="\"$s\""
		return 0
		;;
	esac
	len=${#s}
	while ((i < len)); do
		c=${s:i:1}
		case $c in
		'"') out+='\"' ;;
		'\') out+='\\' ;;
		[\ -~]) out+=$c ;;
		*)
			printf -v n '%d' "'$c"
			if ((n >= 0 && n < 32)); then
				printf -v c '\\u%04x' "$n"
			fi
			out+=$c
			;;
		esac
		i=$((i + 1))
	done
	_hk_json="\"$out\""
}

# _hk_base64: the bytes of stdin, NUL bytes included and one trailing newline dropped, in base64
# in _hk_b64.
_hk_base64() {
	local table='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
	local c n count=0 i=0 out='' bytes
	bytes=()
	while IFS= read -r -d '' -n 1 c; do
		if [[ -z $c ]]; then
			n=0
		else
			printf -v n '%d' "'$c"
			if ((n < 0)); then
				n=$((n + 256))
			fi
		fi
		bytes[count]=$n
		count=$((count + 1))
	done
	# One trailing newline is dropped, as from every input.
	if ((count > 0 && bytes[count - 1] == 10)); then
		count=$((count - 1))
	fi
	while ((i + 2 < count)); do
		n=$((bytes[i] << 16 | bytes[i + 1] << 8 | bytes[i + 2]))
		out+=${table:n >> 18 & 63:1}${table:n >> 12 & 63:1}${table:n >> 6 & 63:1}${table:n & 63:1}
		i=$((i + 3))
	done
	if ((count - i == 2)); then
		n=$((bytes[i] << 16 | bytes[i + 1] << 8))
		out+=${table:n >> 18 & 63:1}${table:n >> 12 & 63:1}${table:n >> 6 & 63:1}=
	elif ((count - i == 1)); then
		n=$((bytes[i] << 16))
		out+=${table:n >> 18 & 63:1}${table:n >> 12 & 63:1}==
	fi
	_hk_b64=$out
}

# _hk_read_file PATH WHAT: the text of the file PATH in _hk_value, one trailing newline dropped.
# WHAT names it in an error; what it holds is never in one.
_hk_read_file() {
	if [[ ! -r $1 || -d $1 ]]; then
		_hk_usage "cannot read $2 from $1"
	fi
	IFS= read -r -d '' _hk_value <"$1"
	local status=$?
	if ((status == 0)); then
		_hk_usage "$2 holds a NUL byte"
	fi
	_hk_value=${_hk_value%$'\n'}
}

# _hk_read_stdin WHAT: the text of stdin in _hk_value, one trailing newline dropped.
_hk_read_stdin() {
	IFS= read -r -d '' _hk_value
	local status=$?
	if ((status == 0)); then
		_hk_usage "$1 holds a NUL byte"
	fi
	_hk_value=${_hk_value%$'\n'}
}

# _hk_trim TEXT: TEXT without leading and trailing white space, in _hk_trimmed.
_hk_trim() {
	local t=$1
	while [[ $t == [$_hk_ws]* ]]; do
		t=${t#?}
	done
	while [[ $t == *[$_hk_ws] ]]; do
		t=${t%?}
	done
	_hk_trimmed=$t
}

# _hk_int TEXT: TEXT as a value of the integer field _hk_f_*, in _hk_json: a JSON number for
# 32 bits, a string for 64, as proto3 JSON writes them.
_hk_int() {
	local digits=$1 neg=0 max what
	case $_hk_f_kind in
	i32) what='a 32-bit signed integer' max=2147483647 ;;
	u32) what='a 32-bit unsigned integer' max=4294967295 ;;
	i64) what='a 64-bit signed integer' max=9223372036854775807 ;;
	*) what='a 64-bit unsigned integer' max=18446744073709551615 ;;
	esac
	case $digits in
	-*)
		neg=1
		digits=${digits#-}
		;;
	esac
	case $digits in
	'' | *[!0-9]*) _hk_usage "--$_hk_f_flag takes $what" ;;
	esac
	while [[ $digits == 0?* ]]; do
		digits=${digits#0}
	done
	if [[ $digits == 0 ]]; then
		neg=0
	fi
	if ((neg)); then
		case $_hk_f_kind in
		i32) max=2147483648 ;;
		i64) max=9223372036854775808 ;;
		*) _hk_usage "--$_hk_f_flag takes $what" ;;
		esac
		digits=-$digits
	fi
	local magnitude=${digits#-}
	if ((${#magnitude} > ${#max})) || { ((${#magnitude} == ${#max})) && [[ $magnitude > $max ]]; }; then
		_hk_usage "--$_hk_f_flag takes $what"
	fi
	case $_hk_f_kind in
	i64 | u64) _hk_json="\"$digits\"" ;;
	*) _hk_json=$digits ;;
	esac
}

# _hk_encode TEXT: TEXT as one value of the field _hk_f_*, in _hk_json.
_hk_encode() {
	case $_hk_f_kind in
	string) _hk_json_string "$1" ;;
	bool)
		case $1 in
		true | false) _hk_json=$1 ;;
		*) _hk_usage "--$_hk_f_flag takes true or false" ;;
		esac
		;;
	enum)
		local list=$_hk_f_enum, item names=''
		while [[ -n $list ]]; do
			item=${list%%,*}
			list=${list#*,}
			if [[ ${item%%=*} == "$1" ]]; then
				_hk_json="\"${item#*=}\""
				return 0
			fi
			names+=${names:+, }${item%%=*}
		done
		_hk_usage "--$_hk_f_flag takes one of: $names"
		;;
	float)
		case $1 in
		NaN | Infinity | -Infinity) ;;
		*)
			if ! [[ $1 =~ $_hk_re_float ]]; then
				_hk_usage "--$_hk_f_flag takes a number"
			fi
			;;
		esac
		_hk_json="\"$1\""
		;;
	*) _hk_int "$1" ;;
	esac
}

# _hk_put INDEX TEXT: one occurrence of the flag of the field INDEX (parsed into _hk_f_*).
_hk_put() {
	local i=$1 text=$2 key
	case $_hk_f_label in
	many)
		_hk_encode "$text"
		if [[ -n ${_hk_set[i]} ]]; then
			_hk_vals[i]+=,$_hk_json
		else
			_hk_vals[i]=$_hk_json
			_hk_set[i]=1
		fi
		;;
	map)
		case $text in
		*=*) ;;
		*) _hk_usage "--$_hk_f_flag takes key=value" ;;
		esac
		_hk_json_string "${text%%=*}"
		key=$_hk_json
		case ,${_hk_mapkeys[i]}, in
		*",$key,"*) _hk_usage "--$_hk_f_flag names the key $key twice" ;;
		esac
		_hk_mapkeys[i]+=${_hk_mapkeys[i]:+,}$key
		_hk_json_string "${text#*=}"
		if [[ -n ${_hk_set[i]} ]]; then
			_hk_vals[i]+=,$key:$_hk_json
		else
			_hk_vals[i]=$key:$_hk_json
			_hk_set[i]=1
		fi
		;;
	*)
		if [[ -n ${_hk_set[i]} ]]; then
			_hk_usage "--$_hk_f_flag is given twice"
		fi
		_hk_encode "$text"
		_hk_vals[i]=$_hk_json
		_hk_set[i]=1
		;;
	esac
}

# _hk_field_flag ARGS...: takes a flag of a request field at the start of ARGS, with its value,
# and sets _hk_took; returns 1 when ARGS do not start with one.
_hk_field_flag() {
	local flag=${1%%=*} i=0 n=${#_hk_spec[@]}
	while ((i < n)); do
		_hk_parse_spec "${_hk_spec[i]}"
		case $_hk_f_input in
		f)
			if [[ $flag == "--$_hk_f_flag" ]]; then
				_hk_arg "$flag" "$@"
				_hk_put "$i" "$_hk_arg_value"
				return 0
			fi
			;;
		s)
			if [[ $flag == "--$_hk_f_flag-file" ]]; then
				_hk_arg "$flag" "$@"
				if [[ -n ${_hk_set[i]} ]]; then
					_hk_usage "--$_hk_f_flag-file and --$_hk_f_flag-stdin: give one, once"
				fi
				_hk_secret_src[i]=$_hk_arg_value
				_hk_set[i]=1
				return 0
			fi
			if [[ $flag == "--$_hk_f_flag-stdin" ]]; then
				_hk_switch "$flag" "$1"
				if [[ -n ${_hk_set[i]} ]]; then
					_hk_usage "--$_hk_f_flag-file and --$_hk_f_flag-stdin: give one, once"
				fi
				_hk_stdin "$flag"
				_hk_secret_src[i]=-
				_hk_set[i]=1
				return 0
			fi
			;;
		esac
		i=$((i + 1))
	done
	return 1
}

# _hk_scan_object TEXT: whether TEXT is one JSON object; its top-level keys as written in
# _hk_keys, their values' text in _hk_kvals, and what is between its braces in _hk_inner.
# _hk_key_escaped is 1 when a key holds an escape. The gateway reads the object itself; this
# finds the keys a flag must not set again.
_hk_scan_object() {
	local s=$1 len=${#1} i=0 c depth=0 instr=0 esc=0 inkey=0 expect=1 keyed=0 key='' vstart=-1 open close=-1 n=0
	_hk_keys=()
	_hk_kvals=()
	_hk_key_escaped=0
	while ((i < len)) && [[ ${s:i:1} == [$_hk_ws] ]]; do
		i=$((i + 1))
	done
	if [[ ${s:i:1} != '{' ]]; then
		return 1
	fi
	open=$i
	depth=1
	i=$((i + 1))
	while ((i < len)); do
		c=${s:i:1}
		if ((instr)); then
			if ((esc)); then
				esc=0
			elif [[ $c == '\' ]]; then
				esc=1
				if ((inkey)); then
					_hk_key_escaped=1
				fi
			elif [[ $c == '"' ]]; then
				instr=0
				if ((inkey)); then
					inkey=0
					_hk_keys[n]=$key
					n=$((n + 1))
					keyed=1
				fi
			elif ((inkey)); then
				key+=$c
			fi
		else
			case $c in
			'"')
				instr=1
				if ((depth == 1 && expect)); then
					inkey=1
					key=''
				fi
				;;
			'{' | '[') depth=$((depth + 1)) ;;
			'}' | ']')
				depth=$((depth - 1))
				if ((depth == 0)); then
					close=$i
					break
				fi
				;;
			':')
				if ((depth == 1)); then
					# A value with no key before it: not an object.
					if ((!keyed)); then
						return 1
					fi
					keyed=0
					expect=0
					vstart=$((i + 1))
				fi
				;;
			',')
				if ((depth == 1 && vstart >= 0)); then
					_hk_trim "${s:vstart:i-vstart}"
					_hk_kvals[n - 1]=$_hk_trimmed
					vstart=-1
					expect=1
				fi
				;;
			esac
		fi
		i=$((i + 1))
	done
	if ((close < 0)); then
		return 1
	fi
	if ((vstart >= 0)); then
		_hk_trim "${s:vstart:close-vstart}"
		_hk_kvals[n - 1]=$_hk_trimmed
	fi
	_hk_trim "${s:close+1}"
	if [[ -n $_hk_trimmed ]]; then
		return 1
	fi
	_hk_trim "${s:open+1:close-open-1}"
	_hk_inner=$_hk_trimmed
	return 0
}

# _hk_build: the request body, in _hk_body: the request file's object with every field a flag
# or a secret gives added to it.
_hk_build() {
	local i j n=${#_hk_spec[@]} k parts value seen name
	parts=()
	seen=()
	_hk_inner=''
	_hk_keys=()
	if ((_hk_request_set)); then
		if [[ $_hk_request == - ]]; then
			_hk_read_stdin 'the request'
		else
			_hk_read_file "$_hk_request" 'the request'
		fi
		if ! _hk_scan_object "$_hk_value"; then
			_hk_usage 'the request file is not a JSON object'
		fi
		if ((_hk_key_escaped)); then
			_hk_usage 'the request file has a key with an escape in it; spell each key plainly'
		fi
	fi
	# Each key of the request file that names a field: once, and not by a flag too.
	k=0
	while ((k < ${#_hk_keys[@]})); do
		j=0
		while ((j < n)); do
			_hk_parse_spec "${_hk_spec[j]}"
			if [[ ${_hk_keys[k]} == "$_hk_f_name" || ${_hk_keys[k]} == "$_hk_f_json" ]]; then
				if [[ -n ${seen[j]} ]]; then
					_hk_usage "the request file names \`$_hk_f_name\` twice, by its proto name and its JSON name"
				fi
				seen[j]=1
				if [[ -n ${_hk_set[j]} ]]; then
					name=--$_hk_f_flag
					if [[ $_hk_f_input == s ]]; then
						name+=-file
					fi
					_hk_usage "$name and the request file both set \`$_hk_f_name\`"
				fi
				if ((j == _hk_hold)); then
					_hk_hold_text=${_hk_kvals[k]}
				fi
			fi
			j=$((j + 1))
		done
		k=$((k + 1))
	done
	i=0
	while ((i < n)); do
		if [[ -n ${_hk_set[i]} ]]; then
			_hk_parse_spec "${_hk_spec[i]}"
			if [[ $_hk_f_input == s ]]; then
				if [[ $_hk_f_kind == bytes ]]; then
					if [[ ${_hk_secret_src[i]} == - ]]; then
						_hk_base64
					elif [[ ! -r ${_hk_secret_src[i]} || -d ${_hk_secret_src[i]} ]]; then
						_hk_usage "cannot read \`$_hk_f_name\` from ${_hk_secret_src[i]}"
					else
						_hk_base64 <"${_hk_secret_src[i]}"
					fi
					_hk_vals[i]="\"$_hk_b64\""
				else
					if [[ ${_hk_secret_src[i]} == - ]]; then
						_hk_read_stdin "\`$_hk_f_name\`"
					else
						_hk_read_file "${_hk_secret_src[i]}" "\`$_hk_f_name\`"
					fi
					_hk_json_string "$_hk_value"
					_hk_vals[i]=$_hk_json
				fi
			fi
			case $_hk_f_label in
			many) value="[${_hk_vals[i]}]" ;;
			map) value="{${_hk_vals[i]}}" ;;
			*) value=${_hk_vals[i]} ;;
			esac
			parts[${#parts[@]}]="\"$_hk_f_json\":$value"
			if ((i == _hk_hold)); then
				_hk_hold_text=${_hk_vals[i]}
			fi
		fi
		i=$((i + 1))
	done
	local IFS=,
	if [[ -n $_hk_inner && ${#parts[@]} -gt 0 ]]; then
		_hk_body="{$_hk_inner,${parts[*]}}"
	else
		_hk_body="{$_hk_inner${parts[*]}}"
	fi
}

# _hk_private PATH: refuses PATH unless only its owner may read it (a POSIX find).
_hk_private() {
	local p=$1 out
	case $p in
	/*) ;;
	*) p=./$p ;;
	esac
	if ! out=$(find -H "$p" -prune \( -perm -040 -o -perm -004 \) -print 2>/dev/null); then
		_hk_usage "cannot read the mode of --token-file $1"
	fi
	if [[ -n $out ]]; then
		_hk_usage "--token-file $1 may be read by its group or others; make it 0600"
	fi
}

# _hk_visible TEXT: whether TEXT is non-empty visible ASCII with no backslash.
_hk_visible() {
	case $1 in
	'' | *[![:graph:]]* | *'\'*) return 1 ;;
	esac
	return 0
}

# _hk_oidc: a fresh GitHub Actions ID token for the audience --github-oidc-audience names, in
# _hk_token, masked in the job's log before it is used.
_hk_oidc() {
	local enc='' c i=0 n status http sep='?'
	if [[ -z $_hk_oidc_url || -z $_hk_oidc_request ]]; then
		_hk_fail '--github-oidc-audience needs the job to have id-token: write: the runner set no ACTIONS_ID_TOKEN_REQUEST_URL and ACTIONS_ID_TOKEN_REQUEST_TOKEN'
	fi
	case $_hk_oidc_url in
	https://*) ;;
	*) _hk_fail 'ACTIONS_ID_TOKEN_REQUEST_URL is not an https URL' ;;
	esac
	if ! _hk_visible "$_hk_oidc_url" || ! _hk_visible "$_hk_oidc_request"; then
		_hk_fail 'the runner set ACTIONS_ID_TOKEN_REQUEST_URL or ACTIONS_ID_TOKEN_REQUEST_TOKEN with a character that is not visible ASCII'
	fi
	while ((i < ${#_hk_audience})); do
		c=${_hk_audience:i:1}
		case $c in
		[A-Za-z0-9._~-]) enc+=$c ;;
		*)
			printf -v n '%d' "'$c"
			if ((n < 0)); then
				n=$((n + 256))
			fi
			printf -v c '%%%02X' "$n"
			enc+=$c
			;;
		esac
		i=$((i + 1))
	done
	case $_hk_oidc_url in
	*'?'*) sep='&' ;;
	esac
	_hk_reply=$(curl -q --silent --show-error --proto =https --tlsv1.2 --max-redirs 0 --globoff \
		--noproxy '*' --connect-timeout "$_hk_timeout" --max-time "$_hk_timeout" \
		--header @<(printf 'Authorization: Bearer %s\n' "$_hk_oidc_request") \
		--write-out '\n%{http_code}' --url "$_hk_oidc_url${sep}audience=$enc")
	status=$?
	if ((status != 0)); then
		_hk_fail "the runner's token request failed (curl exited $status)"
	fi
	http=${_hk_reply##*$'\n'}
	_hk_reply=${_hk_reply%$'\n'*}
	if [[ $http != 200 ]]; then
		_hk_fail "the runner's token request was answered HTTP $http"
	fi
	case $_hk_reply in
	*'"value"'*) ;;
	*) _hk_fail "the runner's token reply has no value" ;;
	esac
	_hk_reply=${_hk_reply#*'"value"'}
	_hk_trim "$_hk_reply"
	_hk_reply=${_hk_trimmed#:}
	_hk_trim "$_hk_reply"
	_hk_reply=${_hk_trimmed#'"'}
	_hk_token=${_hk_reply%%'"'*}
	if [[ $_hk_reply != *'"'* ]] || ! _hk_visible "$_hk_token"; then
		_hk_fail "the runner's token reply has no value that is a token"
	fi
	printf '::add-mask::%s\n' "$_hk_token"
}

# _hk_credential: the bearer token, from exactly one of --token-file, --token-stdin and
# --github-oidc-audience, in _hk_token.
_hk_credential() {
	local given=$((_hk_token_file_set + _hk_token_stdin + _hk_audience_set))
	if ((given != 1)); then
		_hk_usage 'a call needs exactly one of --token-file, --token-stdin and --github-oidc-audience'
	fi
	if ((_hk_audience_set)); then
		_hk_oidc
		return 0
	fi
	if ((_hk_token_stdin)); then
		_hk_read_stdin 'the token'
	else
		_hk_private "$_hk_token_file"
		_hk_read_file "$_hk_token_file" 'the token'
	fi
	_hk_token=$_hk_value
	_hk_value=''
	if ! _hk_visible "$_hk_token"; then
		_hk_usage 'the token is empty or holds a character that is not visible ASCII'
	fi
}

# _hk_older: after a call refused `invalid_request`: whether the coordinator is older than this
# hkctl, said on stderr when it is. One more call, to the coordinator's `GET /v1/version` with
# this hkctl's release as `at_least`, with no token: `409` says it is older and which version it
# runs, `404` is a coordinator before that route. Anything else (the coordinator is not older, no
# answer, an answer not read, a stamp with no version) leaves the refusal as it was; returns 1.
_hk_older() {
	local want=${_hk_version#hkctl } probe http now
	want=${want%%+*}
	if [[ -z $want ]]; then
		return 1
	fi
	probe=$(curl -q --silent --show-error --proto =https --tlsv1.2 --max-redirs 0 --globoff \
		--cacert "$_hk_ca" --connect-timeout "$_hk_timeout" --max-time "$_hk_timeout" \
		--write-out '\n%{http_code}' --url "$_hk_endpoint/v1/version?at_least=$want" 2>/dev/null)
	if (($? != 0)); then
		return 1
	fi
	http=${probe##*$'\n'}
	probe=${probe%$'\n'*}
	now=''
	if [[ $probe =~ $_hk_re_version ]] && _hk_visible "${BASH_REMATCH[1]}"; then
		now=${BASH_REMATCH[1]}
	fi
	case $http in
	409)
		if [[ -n $now ]]; then
			printf 'hkctl: the coordinator is older than this hkctl (%s): it runs %s; update the coordinator, then call again\n' "$want" "$now" >&2
			return 0
		fi
		;;
	404)
		printf 'hkctl: the coordinator is older than this hkctl (%s): it does not say its version; update the coordinator, then call again\n' "$want" >&2
		return 0
		;;
	200)
		if [[ -n $now ]]; then
			printf 'hkctl: the coordinator runs %s, not older than this hkctl (%s)\n' "$now" "$want" >&2
		fi
		;;
	esac
	return 1
}

# _hk_call: the one call of the method _hk_path with the body _hk_body; prints the answer, or
# exits with the code of the refusal.
_hk_call() {
	local status http max code reason
	if ((!_hk_endpoint_set)); then
		_hk_usage 'a call needs --endpoint'
	fi
	if ! [[ $_hk_endpoint =~ $_hk_re_endpoint ]]; then
		_hk_usage '--endpoint takes https://<host>[:<port>]'
	fi
	_hk_endpoint=${_hk_endpoint%/}
	if ((!_hk_ca_set)); then
		_hk_usage 'a call needs --ca-file'
	fi
	if [[ ! -r $_hk_ca || -d $_hk_ca ]]; then
		_hk_usage "cannot read --ca-file $_hk_ca"
	fi
	if ((_hk_response_set)) && [[ -z $_hk_response ]]; then
		_hk_usage '--response-file needs a path'
	fi
	_hk_build
	_hk_credential
	max=$_hk_timeout
	_hk_hold_text=${_hk_hold_text#'"'}
	_hk_hold_text=${_hk_hold_text%'"'}
	if [[ $_hk_hold_text =~ $_hk_re_secs ]]; then
		max=$((max + _hk_hold_text))
	fi
	_hk_reply=$(curl -q --silent --show-error --proto =https --tlsv1.2 --max-redirs 0 --globoff \
		--cacert "$_hk_ca" --connect-timeout "$_hk_timeout" --max-time "$max" \
		--header @<(printf 'Authorization: Bearer %s\n' "$_hk_token") \
		--header 'Content-Type: application/json' \
		--data-binary @<(printf '%s' "$_hk_body") \
		--write-out '\n%{http_code}' --url "$_hk_endpoint$_hk_path")
	status=$?
	if ((status != 0)); then
		_hk_fail "no answer from $_hk_endpoint (curl exited $status)"
	fi
	http=${_hk_reply##*$'\n'}
	_hk_reply=${_hk_reply%$'\n'*}
	if [[ $http == 200 ]]; then
		if ((_hk_response_set)); then
			if ! { printf '%s\n' "$_hk_reply" >"$_hk_response"; } 2>/dev/null; then
				_hk_fail "cannot write --response-file $_hk_response"
			fi
		else
			printf '%s\n' "$_hk_reply"
		fi
		exit 0
	fi
	if ! [[ $_hk_reply =~ $_hk_re_refusal ]]; then
		_hk_fail "the answer (HTTP $http) is not the gateway's"
	fi
	code=${BASH_REMATCH[1]}
	reason=${BASH_REMATCH[2]}
	_hk_exit_of "$code"
	status=$?
	if ((status == 0)); then
		_hk_fail "the answer (HTTP $http) is not the gateway's"
	fi
	printf 'hkctl: the coordinator answered %s\n' "$code" >&2
	if [[ $reason == invalid_request ]] && _hk_older; then
		reason=coordinator_older
	fi
	if [[ $reason =~ $_hk_re_reason ]]; then
		printf 'refused: %s\n' "$reason" >&2
	fi
	exit "$status"
}

# _hk_run ARGS...: the flags of the method _hk_dispatch chose, then its call.
_hk_run() {
	local flag
	_hk_vals=()
	_hk_set=()
	_hk_secret_src=()
	_hk_mapkeys=()
	_hk_request=''
	_hk_request_set=0
	_hk_hold_text=''
	while (($#)); do
		flag=${1%%=*}
		case $flag in
		--help | -h)
			"$_hk_helpfn"
			exit 0
			;;
		--request)
			_hk_arg "$flag" "$@"
			if ((_hk_request_set)); then
				_hk_usage "$flag is given twice"
			fi
			_hk_request_set=1
			_hk_request=$_hk_arg_value
			if [[ $_hk_request == - ]]; then
				_hk_stdin "$flag"
			fi
			shift "$_hk_took"
			continue
			;;
		--*) ;;
		*) _hk_usage "$_hk_command takes no argument; it has flags only" ;;
		esac
		if _hk_field_flag "$@" || _hk_global "$@"; then
			shift "$_hk_took"
		else
			_hk_usage "$_hk_command has no flag $flag"
		fi
	done
	_hk_call
}

# _hk_main ARGS...: hkctl [global flags] <service> <method> [flags].
_hk_main() {
	local service method
	_hk_endpoint=''
	_hk_endpoint_set=0
	_hk_ca=''
	_hk_ca_set=0
	_hk_token=''
	_hk_token_file=''
	_hk_token_file_set=0
	_hk_token_stdin=0
	_hk_audience=''
	_hk_audience_set=0
	_hk_response=''
	_hk_response_set=0
	_hk_timeout=30
	_hk_timeout_set=0
	_hk_stdin_by=''
	while (($#)); do
		case $1 in
		--help | -h)
			_hk_help
			exit 0
			;;
		--version)
			printf '%s\n' "$_hk_version"
			exit 0
			;;
		-*)
			if _hk_global "$@"; then
				shift "$_hk_took"
			else
				_hk_usage "unknown flag ${1%%=*}"
			fi
			;;
		*) break ;;
		esac
	done
	if (($# == 0)); then
		_hk_help >&2
		exit 2
	fi
	service=$1
	shift
	if ! _hk_help_service "$service" >/dev/null; then
		_hk_usage "no service $service; 'hkctl --help' lists them"
	fi
	while (($#)); do
		case $1 in
		--help | -h)
			_hk_help_service "$service"
			exit 0
			;;
		-*)
			if _hk_global "$@"; then
				shift "$_hk_took"
			else
				_hk_usage "unknown flag ${1%%=*}"
			fi
			;;
		*) break ;;
		esac
	done
	if (($# == 0)); then
		_hk_help_service "$service" >&2
		exit 2
	fi
	method=$1
	shift
	if ! _hk_dispatch "$service" "$method"; then
		_hk_usage "the service $service has no method $method; 'hkctl $service --help' lists them"
	fi
	_hk_run "$@"
}
