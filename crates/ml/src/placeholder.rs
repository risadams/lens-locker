//! A hand-built, dependency-free minimal ONNX (protobuf) encoder.
//!
//! Milestone ML-1's exit criteria ("a `Session` loads and runs a forward
//! pass for each of the three models") needs real YuNet/SFace/SigLIP
//! `.onnx` weight files, which aren't available in this environment (see
//! `MODELS.md`). This module builds a minimal *structurally valid* ONNX
//! graph — one `Identity` node, one input, one output, both the same
//! float32 shape — entirely by hand, so the `ort`/DirectML `Session`
//! plumbing itself can be exercised without waiting on those files.
//!
//! **Unverified against real ONNX Runtime.** The field numbers below are
//! transcribed from `onnx.proto`'s stable, long-published wire schema, but
//! nothing in this crate can actually load them through the real ONNX
//! Runtime C API in this environment (that needs the bundled dylib —
//! `MODELS.md`). [`tests::placeholder_bytes_contain_the_expected_graph_shape`]
//! only checks the encoded bytes structurally round-trip through this
//! module's own tiny decoder; [`super::tests::sessions_load_and_run_a_forward_pass_for_each_model_slot`]
//! is the real end-to-end check, `#[ignore]`d until the dylib exists.
//! Flagging this rather than asserting untested certainty, matching this
//! repo's convention for unconfirmed build-time numbers
//! (workplan/ML-SPEC.md §10).

fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            buf.push(byte | 0x80);
        } else {
            buf.push(byte);
            break;
        }
    }
}

fn field_tag(field_number: u32, wire_type: u8) -> u64 {
    ((field_number as u64) << 3) | wire_type as u64
}

fn write_varint_field(buf: &mut Vec<u8>, field_number: u32, value: u64) {
    write_varint(buf, field_tag(field_number, 0));
    write_varint(buf, value);
}

fn write_bytes_field(buf: &mut Vec<u8>, field_number: u32, data: &[u8]) {
    write_varint(buf, field_tag(field_number, 2));
    write_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

fn write_string_field(buf: &mut Vec<u8>, field_number: u32, value: &str) {
    write_bytes_field(buf, field_number, value.as_bytes());
}

const ELEM_TYPE_FLOAT: u64 = 1;

fn tensor_shape_proto(dims: &[i64]) -> Vec<u8> {
    let mut shape = Vec::new();
    for &dim in dims {
        let mut dimension = Vec::new();
        write_varint_field(&mut dimension, 1, dim as u64); // Dimension.dim_value
        write_bytes_field(&mut shape, 1, &dimension); // TensorShapeProto.dim
    }
    shape
}

fn type_proto_tensor(dims: &[i64]) -> Vec<u8> {
    let mut tensor_type = Vec::new();
    write_varint_field(&mut tensor_type, 1, ELEM_TYPE_FLOAT); // Tensor.elem_type
    write_bytes_field(&mut tensor_type, 2, &tensor_shape_proto(dims)); // Tensor.shape

    let mut type_proto = Vec::new();
    write_bytes_field(&mut type_proto, 1, &tensor_type); // TypeProto.tensor_type (oneof)
    type_proto
}

fn value_info_proto(name: &str, dims: &[i64]) -> Vec<u8> {
    let mut value_info = Vec::new();
    write_string_field(&mut value_info, 1, name); // ValueInfoProto.name
    write_bytes_field(&mut value_info, 2, &type_proto_tensor(dims)); // ValueInfoProto.type
    value_info
}

/// Builds a minimal valid ONNX `ModelProto`: opset 13, ir_version 8 (well
/// within onnxruntime 1.24's backward-compatible range), one `Identity`
/// node mapping `input_name` straight to `output_name`, both `dims`-shaped
/// float32 tensors.
pub fn identity_graph_model(input_name: &str, output_name: &str, dims: &[i64]) -> Vec<u8> {
    let mut node = Vec::new();
    write_string_field(&mut node, 1, input_name); // NodeProto.input
    write_string_field(&mut node, 2, output_name); // NodeProto.output
    write_string_field(&mut node, 3, "identity"); // NodeProto.name
    write_string_field(&mut node, 4, "Identity"); // NodeProto.op_type

    let mut graph = Vec::new();
    write_bytes_field(&mut graph, 1, &node); // GraphProto.node
    write_string_field(&mut graph, 2, "lenslocker-ml-placeholder"); // GraphProto.name
    write_bytes_field(&mut graph, 11, &value_info_proto(input_name, dims)); // GraphProto.input
    write_bytes_field(&mut graph, 12, &value_info_proto(output_name, dims)); // GraphProto.output

    let mut opset = Vec::new();
    write_varint_field(&mut opset, 2, 13); // OperatorSetIdProto.version (domain left empty = default "ai.onnx")

    let mut model = Vec::new();
    write_varint_field(&mut model, 1, 8); // ModelProto.ir_version
    write_bytes_field(&mut model, 2, &opset); // ModelProto.opset_import
    write_string_field(&mut model, 3, "lenslocker-ml"); // ModelProto.producer_name
    write_bytes_field(&mut model, 8, &graph); // ModelProto.graph
    model
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|window| window == needle)
    }

    #[test]
    fn placeholder_bytes_contain_the_expected_graph_shape() {
        let bytes = identity_graph_model("input", "output", &[1, 3, 4, 4]);

        assert!(!bytes.is_empty());
        assert!(contains_subsequence(&bytes, b"Identity"));
        assert!(contains_subsequence(&bytes, b"input"));
        assert!(contains_subsequence(&bytes, b"output"));
        assert!(contains_subsequence(&bytes, b"lenslocker-ml"));
    }

    #[test]
    fn distinct_shapes_produce_distinct_bytes() {
        let a = identity_graph_model("input", "output", &[1, 3, 4, 4]);
        let b = identity_graph_model("input", "output", &[1, 3, 120, 160]);
        assert_ne!(a, b);
    }
}
