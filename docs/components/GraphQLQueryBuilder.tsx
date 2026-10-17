// Issue #202: Automated GraphQL Query Builder & Code Generator Component
// Visual query builder with real-time code generation

import React, { useState, useEffect } from 'react';
import {
  buildSchema,
  GraphQLSchema,
  GraphQLObjectType,
  GraphQLField,
  GraphQLType,
  isObjectType,
  isScalarType,
  isListType,
  isNonNullType,
  GraphQLOutputType,
} from 'graphql';

interface FieldSelection {
  name: string;
  selected: boolean;
  children?: FieldSelection[];
  args?: Record<string, any>;
}

interface GraphQLQueryBuilderProps {
  schemaSDL: string;
  endpoint: string;
  authToken?: string;
}

export const GraphQLQueryBuilder: React.FC<GraphQLQueryBuilderProps> = ({
  schemaSDL,
  endpoint,
  authToken,
}) => {
  const [schema, setSchema] = useState<GraphQLSchema | null>(null);
  const [queryType, setQueryType] = useState<GraphQLObjectType | null>(null);
  const [selectedFields, setSelectedFields] = useState<FieldSelection[]>([]);
  const [generatedQuery, setGeneratedQuery] = useState('');
  const [codeLanguage, setCodeLanguage] = useState<'curl' | 'javascript' | 'python' | 'rust'>('javascript');
  const [queryResult, setQueryResult] = useState<any>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Parse schema
  useEffect(() => {
    try {
      const parsedSchema = buildSchema(schemaSDL);
      setSchema(parsedSchema);
      const rootQueryType = parsedSchema.getQueryType();
      if (rootQueryType) {
        setQueryType(rootQueryType);
        initializeFields(rootQueryType);
      }
    } catch (err) {
      setError(`Failed to parse schema: ${err}`);
    }
  }, [schemaSDL]);

  // Initialize field selections
  const initializeFields = (type: GraphQLObjectType) => {
    const fields = Object.values(type.getFields());
    const fieldSelections: FieldSelection[] = fields.map((field) => ({
      name: field.name,
      selected: false,
      children: getFieldChildren(field.type),
    }));
    setSelectedFields(fieldSelections);
  };

  // Get child fields for a type
  const getFieldChildren = (type: GraphQLOutputType): FieldSelection[] | undefined => {
    let unwrappedType = type;

    // Unwrap NonNull and List wrappers
    while (isNonNullType(unwrappedType) || isListType(unwrappedType)) {
      unwrappedType = unwrappedType.ofType;
    }

    if (isObjectType(unwrappedType)) {
      const fields = Object.values(unwrappedType.getFields());
      return fields.map((field) => ({
        name: field.name,
        selected: false,
        children: getFieldChildren(field.type),
      }));
    }

    return undefined;
  };

  // Toggle field selection
  const toggleField = (path: number[]) => {
    const newFields = [...selectedFields];
    let current: FieldSelection[] = newFields;

    for (let i = 0; i < path.length - 1; i++) {
      if (current[path[i]].children) {
        current = current[path[i]].children!;
      }
    }

    current[path[path.length - 1]].selected = !current[path[path.length - 1]].selected;
    setSelectedFields(newFields);
  };

  // Generate GraphQL query
  useEffect(() => {
    const query = buildGraphQLQuery(selectedFields);
    setGeneratedQuery(query);
  }, [selectedFields]);

  const buildGraphQLQuery = (fields: FieldSelection[], indent = 0): string => {
    const indentation = '  '.repeat(indent);
    const selectedFieldsOnly = fields.filter((f) => f.selected);

    if (selectedFieldsOnly.length === 0) return '';

    const fieldStrings = selectedFieldsOnly.map((field) => {
      if (field.children && field.children.some((c) => c.selected)) {
        const childQuery = buildGraphQLQuery(field.children, indent + 1);
        return `${indentation}  ${field.name} {\n${childQuery}\n${indentation}  }`;
      }
      return `${indentation}  ${field.name}`;
    });

    if (indent === 0) {
      return `query {\n${fieldStrings.join('\n')}\n}`;
    }

    return fieldStrings.join('\n');
  };

  // Generate code snippets
  const generateCode = (language: string): string => {
    const query = generatedQuery;
    const escapedQuery = query.replace(/\n/g, '\\n').replace(/"/g, '\\"');

    switch (language) {
      case 'curl':
        return `curl -X POST ${endpoint} \\
  -H "Content-Type: application/json" \\
  ${authToken ? `-H "Authorization: Bearer ${authToken}" \\\n  ` : ''}-d '{"query":"${escapedQuery}"}'`;

      case 'javascript':
        return `fetch('${endpoint}', {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    ${authToken ? `'Authorization': 'Bearer ${authToken}',\n    ` : ''} },
  body: JSON.stringify({
    query: \`${query}\`
  })
})
  .then(res => res.json())
  .then(data => console.log(data))
  .catch(err => console.error(err));`;

      case 'python':
        return `import requests
import json

query = """${query}"""

response = requests.post(
    '${endpoint}',
    json={'query': query},
    headers={
        'Content-Type': 'application/json',
        ${authToken ? `'Authorization': 'Bearer ${authToken}',\n        ` : ''} }
)

data = response.json()
print(json.dumps(data, indent=2))`;

      case 'rust':
        return `use reqwest;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = r#"${query}"#;
    
    let client = reqwest::Client::new();
    let response = client
        .post("${endpoint}")
        .header("Content-Type", "application/json")
        ${authToken ? `.header("Authorization", "Bearer ${authToken}")\n        ` : ''}.json(&json!({"query": query}))
        .send()
        .await?;
    
    let data = response.json::<serde_json::Value>().await?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    
    Ok(())
}`;

      default:
        return '';
    }
  };

  // Execute query
  const executeQuery = async () => {
    setLoading(true);
    setError(null);
    setQueryResult(null);

    try {
      const response = await fetch(endpoint, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          ...(authToken && { Authorization: `Bearer ${authToken}` }),
        },
        body: JSON.stringify({ query: generatedQuery }),
      });

      const data = await response.json();

      if (data.errors) {
        setError(JSON.stringify(data.errors, null, 2));
      } else {
        setQueryResult(data.data);
      }
    } catch (err) {
      setError(`Query execution failed: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  // Render field tree
  const renderFieldTree = (fields: FieldSelection[], path: number[] = []) => {
    return (
      <div className="ml-4">
        {fields.map((field, index) => {
          const currentPath = [...path, index];
          return (
            <div key={field.name} className="my-1">
              <label className="flex items-center cursor-pointer hover:bg-gray-50 p-1 rounded">
                <input
                  type="checkbox"
                  checked={field.selected}
                  onChange={() => toggleField(currentPath)}
                  className="mr-2"
                />
                <span className="text-sm font-mono">{field.name}</span>
              </label>
              {field.children && field.selected && renderFieldTree(field.children, currentPath)}
            </div>
          );
        })}
      </div>
    );
  };

  return (
    <div className="graphql-query-builder grid grid-cols-2 gap-4 p-4">
      {/* Left Panel - Field Selection */}
      <div className="border rounded-lg p-4 bg-white">
        <h2 className="text-lg font-bold mb-4">Select Fields</h2>
        <div className="overflow-y-auto max-h-[600px]">
          {queryType ? (
            renderFieldTree(selectedFields)
          ) : (
            <p className="text-gray-500">Loading schema...</p>
          )}
        </div>
      </div>

      {/* Right Panel - Code Generation */}
      <div className="space-y-4">
        {/* Generated Query */}
        <div className="border rounded-lg p-4 bg-white">
          <h2 className="text-lg font-bold mb-2">Generated Query</h2>
          <pre className="bg-gray-900 text-gray-100 p-3 rounded text-sm overflow-x-auto">
            {generatedQuery || '// Select fields to generate query'}
          </pre>
        </div>

        {/* Code Snippets */}
        <div className="border rounded-lg p-4 bg-white">
          <div className="flex items-center justify-between mb-2">
            <h2 className="text-lg font-bold">Code Snippet</h2>
            <select
              value={codeLanguage}
              onChange={(e) => setCodeLanguage(e.target.value as any)}
              className="border rounded px-2 py-1 text-sm"
            >
              <option value="javascript">JavaScript</option>
              <option value="curl">cURL</option>
              <option value="python">Python</option>
              <option value="rust">Rust</option>
            </select>
          </div>
          <pre className="bg-gray-900 text-gray-100 p-3 rounded text-sm overflow-x-auto">
            {generateCode(codeLanguage)}
          </pre>
        </div>

        {/* Execute Button */}
        <button
          onClick={executeQuery}
          disabled={loading || !generatedQuery}
          className="w-full bg-blue-500 text-white py-2 rounded hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {loading ? 'Executing...' : 'Execute Query'}
        </button>

        {/* Response Panel */}
        {(queryResult || error) && (
          <div className="border rounded-lg p-4 bg-white">
            <h2 className="text-lg font-bold mb-2">
              {error ? 'Error' : 'Response'}
            </h2>
            <pre
              className={`p-3 rounded text-sm overflow-x-auto ${
                error
                  ? 'bg-red-50 text-red-900'
                  : 'bg-green-50 text-green-900'
              }`}
            >
              {error || JSON.stringify(queryResult, null, 2)}
            </pre>
          </div>
        )}
      </div>
    </div>
  );
};

export default GraphQLQueryBuilder;
